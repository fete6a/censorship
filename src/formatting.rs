//! Telegram MarkdownV2 presentation: untrusted text is always escaped.

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ReviewLine {
    pub message_id: i64,
    pub status: String,
    pub category: String,
    pub probability: f64,
}

pub fn escape_md(text: &str) -> String {
    let mut escaped=String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch,'_'|'*'|'['|']'|'('|')'|'~'|'`'|'>'|'#'|'+'|'-'|'='|'|'|'{'|'}'|'.'|'!'|'\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

pub fn plain_notice(title: &str, text: &str) -> String {
    format!("🛡️ *{}*\n\n{}",escape_md(title),escape_md(text))
}

pub fn message_link(chat_id: i64, message_id: i64) -> Option<String> {
    if message_id<=0 {return None;}
    let group=chat_id.to_string();
    let internal=group.strip_prefix("-100")?;
    if internal.is_empty() || !internal.chars().all(|c|c.is_ascii_digit()) {return None;}
    Some(format!("https://t.me/c/{internal}/{message_id}"))
}

pub fn user_mention(user_id: i64) -> Option<String> {
    if user_id<=0 {return None;}
    Some(format!("[用户 {}](tg://user?id={user_id})",user_id))
}

pub fn action_notice(title: &str, message_id: i64, user_id: Option<i64>, body: &str) -> String {
    let mut out=format!("⚠️ *{}*\n",escape_md(title));
    if message_id>0 {out.push_str(&format!("🔎 消息：`#{message_id}`\n"));}
    if let Some(user)=user_id.and_then(user_mention) {
        out.push_str(&format!("👤 {user}\n"));
    }
    out.push('\n');
    out.push_str(&escape_md(body));
    out
}

fn status_name(status: &str) -> (&'static str, &'static str) {
    match status {
        "allow" => ("✅","通过"),
        "deleted" => ("🗑️","已删除"),
        "needs_review" => ("🔎","人工复核"),
        "dry_run_flag" => ("🧪","模拟命中"),
        "audit_flag" => ("📖","审计命中"),
        "review_error"|"policy_unavailable" => ("⚠️","审核暂不可用"),
        "no_group_description" => ("⚙️","缺少群规"),
        "previously_deleted" => ("↩️","已处理"),
        "too_old_to_delete" => ("⌛","超过删除时限"),
        _ => ("ℹ️","审核状态"),
    }
}

fn row_markdown(chat_id: i64, row: &ReviewLine) -> String {
    let (icon,label)=status_name(&row.status);
    let target=if row.status=="deleted" || row.status=="previously_deleted" {
        format!("`#{}`",row.message_id)
    } else {
        match message_link(chat_id,row.message_id) {
            Some(link)=>format!("[消息 \\#{}]({})",row.message_id,link),
            None=>format!("`#{}`",row.message_id),
        }
    };
    let cat=escape_md(&row.category);
    let percent=escape_md(&format!("{:.1}%",100.0*row.probability.clamp(0.0,1.0)));
    format!("{icon} {target} · *{label}* · {cat} · {percent}")
}

/// Chunk to below Telegram's 4096 UTF-16 unit limit. The Queue may deliver
/// earlier when max_batch_size is reached; this is a batch summary, not a
/// promise that the messages were exactly ten seconds apart.
pub fn review_digest(chat_id: i64, rows: &[ReviewLine]) -> Vec<String> {
    if rows.is_empty(){return vec![];}
    let mut result=Vec::new();
    let mut body=String::new();
    let mut body_units=0;
    let mut count=0;
    let flush = |body: &mut String, count: &mut usize, result: &mut Vec<String>| {
        if *count>0 {
            result.push(format!("📋 *审核摘要* · {} 条\n群组：`{}`\n\n{}",count,chat_id,body));
            body.clear();
            *count=0;
        }
    };
    for item in rows {
        let line=row_markdown(chat_id,item);
        let line_units=line.encode_utf16().count()+1;
        if count>0 && body_units+line_units>3200 {
            flush(&mut body,&mut count,&mut result);
            body_units=0;
        }
        body.push_str(&line);
        body.push('\n');
        body_units+=line_units;
        count+=1;
    }
    flush(&mut body,&mut count,&mut result);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes_untrusted_markdown() {
        let original="*x* [evil](https://bad.example) _test_ ! \\";
        let e=escape_md(original);
        assert!(e.contains("\\*x\\*"));
        assert!(e.contains("\\[evil\\]"));
        assert!(e.contains("\\("));
        assert!(e.contains("\\_test\\_"));
        assert!(e.ends_with("\\\\"));
    }
    #[test]
    fn deep_links_are_numeric_group_scoped() {
        assert_eq!(message_link(-1003783925191,33197).as_deref(),
            Some("https://t.me/c/3783925191/33197"));
        assert!(message_link(-101,33197).is_none());
        assert!(message_link(123,2).is_none());
        assert!(message_link(-100123,0).is_none());
        assert_eq!(user_mention(22).as_deref(),Some("[用户 22](tg://user?id=22)"));
        assert!(user_mention(-22).is_none());
    }
    #[test]
    fn digest_escapes_category_and_does_not_link_deleted_content() {
        let rows=vec![
            ReviewLine{message_id:50,status:"allow".into(),category:"fraud_[x]".into(),probability:0.78},
            ReviewLine{message_id:51,status:"deleted".into(),category:"spam".into(),probability:0.96},
        ];
        let out=review_digest(-100123,&rows);
        assert_eq!(out.len(),1);
        assert!(out[0].contains("[消息 \\#50](https://t.me/c/123/50)"));
        assert!(out[0].contains("fraud\\_\\[x\\]"));
        assert!(out[0].contains("`#51`"));
        assert!(!out[0].contains("/51)"));
        assert!(out[0].contains("96\\.0%"));
    }
    #[test]
    fn digest_chunks_avoid_telegram_limit() {
        let rows=(1..=200).map(|id|ReviewLine{
            message_id:id,status:"needs_review".into(),
            category:"very_long_category_[]".repeat(4),probability:0.95
        }).collect::<Vec<_>>();
        let output=review_digest(-100123,&rows);
        assert!(output.len()>1);
        for chunk in &output {assert!(chunk.encode_utf16().count()<4096);}
    }
}

