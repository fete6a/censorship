//! Telegram MarkdownV2 presentation: untrusted text is always escaped.

#[derive(Clone, Debug, serde::Deserialize)]
pub struct ReviewLine {
    pub message_id: i64,
    pub status: String,
    pub category: String,
    pub probability: f64,
}

pub struct ActionUser {
    pub id: i64,
    pub name: String,
    pub warnings: Option<i64>,
}

/// Fixed-width Unicode bars are code-formatted for Telegram MarkdownV2.
pub fn progress_bar(value: f64, total: f64, width: usize) -> String {
    if !value.is_finite() || !total.is_finite() || total<=0.0 {return "—".into();}
    let width=width.min(20);
    let filled=((value/total).clamp(0.0,1.0)*width as f64).round() as usize;
    format!("{}{}","▰".repeat(filled),"▱".repeat(width-filled))
}

pub fn named_user(id: i64, name: &str) -> Option<String> {
    if id<=0 {return None;}
    let name=name.trim().chars().take(128).collect::<String>();
    if name.is_empty() {return user_mention(id);}
    Some(format!("[{}](tg://user?id={id})",escape_md(&name)))
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

/// The creator comes from the same fresh administrator snapshot as recipients.
/// Names are display text only; profile links use a positive numeric Telegram ID.
pub fn owner_heading(user_id: Option<i64>, first_name: &str, last_name: &str) -> String {
    let Some(id)=user_id.filter(|id|*id>0) else {
        return "👑 群主：暂不可用\n\n".into();
    };
    let name=format!("{} {}",first_name.trim(),last_name.trim());
    let name=name.trim().chars().take(128).collect::<String>();
    format!("👑 群主：{}\n\n",named_user(id,&name).unwrap_or_default())
}

/// Only Telegram-generated usernames/invites or numeric member-only message
/// links become targets. Never interpolate group titles into a URL.
pub fn group_heading(chat_id: i64, title: &str, username: &str, invite: &str, anchor: Option<i64>) -> String {
    let title=title.chars().take(128).collect::<String>();
    let title=if title.trim().is_empty() {"群组".into()} else {title};
    let public=!username.is_empty() && username.len()<=32 &&
        username.bytes().all(|c|c.is_ascii_alphanumeric() || c==b'_');
    let invite_hash=invite.strip_prefix("https://t.me/+")
        .or_else(||invite.strip_prefix("https://t.me/joinchat/"));
    let valid_invite=invite_hash.is_some_and(|hash|!hash.is_empty() && hash.len()<=128 &&
        hash.bytes().all(|c|c.is_ascii_alphanumeric() || c==b'_' || c==b'-'));
    let link=if public {Some(format!("https://t.me/{username}"))}
        else if valid_invite {Some(invite.to_owned())}
        else {anchor.and_then(|id|message_link(chat_id,id))};
    let name=escape_md(&title);
    let label=link.map(|url|format!("[{name}]({url})")).unwrap_or(name);
    format!("👥 {label}\n🆔 `{chat_id}`\n\n")
}

pub fn action_notice(title: &str, message_id: i64, user: Option<&ActionUser>, body: &str) -> String {
    let mut out=format!("⚠️ *{}*\n",escape_md(title));
    if message_id>0 {out.push_str(&format!("🔎 消息：`#{message_id}`\n"));}
    if let Some(user)=user {
        if let Some(mention)=named_user(user.id,&user.name) {out.push_str(&format!("👤 {mention}\n"));}
        if let Some(count)=user.warnings {
            out.push_str(&format!("近 7 天警告：`{}/3` `{}`\n",count.max(0),progress_bar(count.max(0) as f64,3.0,3)));
        }
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

pub fn scan_result(row: &ReviewLine) -> String {
    let (_,label)=status_name(&row.status);
    if matches!(row.status.as_str(),"review_error"|"policy_unavailable"|"no_group_description") {
        return format!("消息 {}：{label}。未获得有效审核结果，未执行处罚。",row.message_id);
    }
    let category=if row.category=="none" {String::new()} else {format!(" · {}",category_name(&row.category))};
    format!("消息 {}：{label}{category}\n违规概率 {} {:.1}%",
        row.message_id,progress_bar(row.probability,1.0,10),100.0*row.probability.clamp(0.0,1.0))
}

pub fn category_name(category: &str) -> &str {
    match category {
        "none"=>"无", "spam"=>"广告/刷屏", "fraud"=>"诈骗/钓鱼",
        "harassment"=>"骚扰/人身攻击", "hate"=>"仇恨攻击", "adult"=>"不当成人内容",
        "threat"=>"威胁", "other"=>"其他违反群规", _=>category,
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
    if matches!(row.status.as_str(),"review_error"|"policy_unavailable"|"no_group_description") {
        return format!("{icon} {target} · *{label}*");
    }
    let cat=if row.category=="none" {String::new()} else {format!(" · {}",escape_md(category_name(&row.category)))};
    let percent=escape_md(&format!("{:.1}%",100.0*row.probability.clamp(0.0,1.0)));
    format!("{icon} {target} · *{label}*{cat}\n违规概率 `{}` *{percent}*",progress_bar(row.probability,1.0,10))
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
            result.push(format!("📋 *审核摘要* · {} 条\n\n{}",count,body));
            body.clear();
            *count=0;
        }
    };
    for item in rows {
        let line=row_markdown(chat_id,item);
        let line_units=line.encode_utf16().count()+1;
        // Leave room for the group heading and escaped creator display name.
        if count>0 && body_units+line_units>2800 {
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
    #[test]
    fn owner_names_are_escaped_and_links_only_use_positive_ids() {
        assert_eq!(owner_heading(Some(42),"Alice *[x]*","Smith"),
            "👑 群主：[Alice \\*\\[x\\]\\* Smith](tg://user?id=42)\n\n");
        assert!(owner_heading(Some(42),"","").contains("[用户 42]"));
        assert!(!owner_heading(Some(-1),"bad","").contains("tg://"));
        assert!(owner_heading(None,"","").contains("暂不可用"));
    }
    #[test]
    fn digest_leaves_space_for_maximum_group_and_owner_headings() {
        let heading=format!("{}{}",
            group_heading(-100123,&"*".repeat(128),"",&format!("https://t.me/+{}","a".repeat(128)),None),
            owner_heading(Some(i64::MAX),&"*".repeat(128),""));
        let rows=(1..=200).map(|id|ReviewLine{
            message_id:id,status:"needs_review".into(),category:"harassment".into(),probability:0.95
        }).collect::<Vec<_>>();
        for chunk in review_digest(-100123,&rows) {
            assert!(format!("{heading}{chunk}").encode_utf16().count()<4096);
        }
    }
    #[test]
    fn scan_errors_are_never_presented_as_successful_reviews() {
        for status in ["review_error","policy_unavailable","no_group_description"] {
            let row=ReviewLine{message_id:7,status:status.into(),category:"none".into(),probability:0.0};
            let output=scan_result(&row);
            assert!(output.contains("未获得有效审核结果"));
            assert!(!output.contains("通过"));
            assert!(!output.contains("违规概率"));
        }
    }
    #[test]
    fn progress_bars_clamp_at_limits_and_keep_counts_separate() {
        assert_eq!(progress_bar(0.0,3.0,3),"▱▱▱");
        assert_eq!(progress_bar(1.0,3.0,3),"▰▱▱");
        assert_eq!(progress_bar(2.0,3.0,3),"▰▰▱");
        assert_eq!(progress_bar(5.0,3.0,3),"▰▰▰");
        assert_eq!(progress_bar(-1.0,3.0,3),"▱▱▱");
        assert_eq!(progress_bar(f64::NAN,1.0,10),"—");
        assert_eq!(progress_bar(0.5,0.0,10),"—");
    }
}

