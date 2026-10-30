//! Bounded, best-effort link evidence obtained with the Worker's native fetch.
//! A link/page is untrusted data, never a source of moderation instructions.
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::Duration;
use futures_util::future::{select, Either};
use serde_json::{json, Value};
use worker::{AbortController, AbortSignal, Delay, Fetch, Headers, Method, Request, RequestInit, RequestRedirect, Url};
use crate::body::response_bytes;
use crate::util::field_str;

// At most two safe targets may use network fetches per message.
const MAX_URLS: usize = 2;
// Blocked URLs are status-only evidence and must not consume fetch slots.
const MAX_BLOCKED_URLS: usize = 2;
const MAX_URL_LEN: usize = 2048;
const MAX_PAGE_BYTES: usize = 256 * 1024;
const MAX_EXCERPT: usize = 2400;
const MAX_REDIRECTS: usize = 2;
// An individual page may not hold later Queue messages indefinitely.
const MAX_PAGE_DURATION_SECS: u64 = 8;

fn parse_url(input: &str, allow_bare: bool) -> Option<Url> {
    let trimmed = input.trim_matches(|c: char| matches!(c,
        ' ' | '\n' | '\r' | '\t' | '<' | '>' | '"' | '\'' |
        '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '!' | '.'));
    if trimmed.len() > MAX_URL_LEN || trimmed.is_empty() { return None; }
    let candidate = if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        trimmed.to_owned()
    } else if trimmed.starts_with("www.") || (allow_bare && trimmed.contains('.')) {
        format!("https://{trimmed}")
    } else {
        return None;
    };
    Url::parse(&candidate).ok().filter(|u| matches!(u.scheme(),"http"|"https"))
}

fn safe_target(url: &Url) -> bool {
    if !matches!(url.scheme(), "http"|"https") ||
        !url.username().is_empty() || url.password().is_some() ||
        url.port().is_some() { return false; }
    let Some(host) = url.host_str() else { return false; };
    if host.len()>253 || host.parse::<IpAddr>().is_ok() || !host.contains('.') ||
        !host.is_ascii() || host.ends_with('.') { return false; }
    // Reject local/special-use names and malformed DNS names. The Cloudflare
    // network also enforces restrictions on destinations; this validation
    // is not intended to be a DNS resolver or a guarantee against rebinding.
    if ["localhost","local","internal","lan","home","test","invalid","example",
        "onion","arpa","corp"].iter().any(|suffix|host==*suffix ||
            host.ends_with(&format!(".{suffix}"))) { return false; }
    host.split('.').all(|label| !label.is_empty() && label.len()<=63 &&
        !label.starts_with('-') && !label.ends_with('-') &&
        label.bytes().all(|b|b.is_ascii_alphanumeric() || b==b'-'))
}

fn utf16_span(text: &str, offset: usize, length: usize) -> Option<String> {
    if length==0 || length>MAX_URL_LEN || offset>text.len()*2 {return None;}
    let units:Vec<u16>=text.encode_utf16().collect();
    let end=offset.checked_add(length)?;
    String::from_utf16(units.get(offset..end)?).ok()
}

fn collect(msg: &Value) -> Vec<Url> {
    let mut seen=BTreeSet::new();
    let mut urls=Vec::new();
    let mut safe_count=0;
    let mut blocked_count=0;
    let mut add=|raw:&str, bare:bool| {
        // Entity counts (32) and whitespace tokens (256) below bound the scan.
        // Rejected targets can add a limited status, but never spend a fetch slot.
        if safe_count>=MAX_URLS && blocked_count>=MAX_BLOCKED_URLS {return;}
        if let Some(url)=parse_url(raw,bare) {
            let safe=safe_target(&url);
            if (safe && safe_count>=MAX_URLS) ||
                (!safe && blocked_count>=MAX_BLOCKED_URLS) {return;}
            let key=url.as_str().to_string();
            if seen.insert(key) {
                if safe {safe_count+=1;} else {blocked_count+=1;}
                urls.push(url);
            }
        }
    };
    for (text_key, entities_key) in [("text","entities"),("caption","caption_entities")] {
        let text=field_str(msg,text_key);
        if let Some(entities)=msg[entities_key].as_array() {
            for entity in entities.iter().take(32) {
                match field_str(entity,"type") {
                    "text_link" => add(field_str(entity,"url"),true),
                    "url" => if let (Some(start),Some(length))=(entity["offset"].as_u64(),entity["length"].as_u64()) {
                        if let Some(raw)=utf16_span(text,start as usize,length as usize) {add(&raw,true);}
                    },
                    _=>{},
                }
            }
        }
        for raw in text.split_whitespace().take(256) {add(raw,false);}
    }
    urls
}

fn public_url(url:&Url)->String {
    let mut safe=url.clone();
    // Display-only copy: never sanitize the target before safe_target(),
    // which must still reject credentials rather than fetching anonymously.
    safe.set_password(None).expect("HTTP(S) URL supports removing password");
    safe.set_username("").expect("HTTP(S) URL supports removing username");
    safe.set_query(None);
    safe.set_fragment(None);
    safe.as_str().chars().take(350).collect()
}

fn excerpt(bytes:&[u8], html:bool)->String {
    let text=String::from_utf8_lossy(bytes);
    let mut out=String::new();
    let mut in_tag=false;
    let mut space=false;
    for ch in text.chars() {
        if html && ch=='<' {in_tag=true;continue;}
        if html && ch=='>' {in_tag=false;space=true;continue;}
        if in_tag {continue;}
        if ch.is_control() || ch.is_whitespace() {
            if !space && !out.is_empty() {out.push(' ');space=true;}
        } else {
            out.push(ch);space=false;
        }
        if out.len()>=MAX_EXCERPT {break;}
    }
    out.trim().to_owned()
}

/// Fetch a safe URL with its original SSRF, redirect, and response size checks.
/// The caller's signal applies to the entire redirect chain and streamed body.
async fn fetch_page(url: Url, displayed: String, signal: &AbortSignal) -> Value {
    let mut current=url;
    let mut result=json!({"url":displayed,"status":"unavailable"});
    for hop in 0..=MAX_REDIRECTS {
        let headers=Headers::new();
        if headers.set("Accept","text/html,text/plain,application/json;q=0.8").is_err() ||
            headers.set("Range","bytes=0-262143").is_err() {break;}
        let mut init=RequestInit::new();
        init.with_method(Method::Get).with_headers(headers).with_redirect(RequestRedirect::Manual);
        let Ok(request)=Request::new_with_init(current.as_str(),&init) else {break;};
        let Ok(mut response)=Fetch::Request(request).send_with_signal(signal).await else {break;};
        let code=response.status_code();
        if (300..400).contains(&code) {
            if hop==MAX_REDIRECTS {
                result["status"]=json!("redirect_limit");
                break;
            }
            let next=response.headers().get("Location").ok().flatten()
                .and_then(|location|current.join(&location).ok());
            if let Some(next)=next {
                if !safe_target(&next) {
                    result["status"]=json!("blocked_redirect");
                    break;
                }
                current=next;
                continue;
            }
            result["status"]=json!("invalid_redirect");
            break;
        }
        result["final_url"]=json!(public_url(&current));
        result["http_status"]=json!(code);
        if !(200..300).contains(&code) {
            result["status"]=json!("http_error");break;
        }
        let mime=response.headers().get("Content-Type").ok().flatten().unwrap_or_default();
        let mime=mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        if !(mime.starts_with("text/") || mime=="application/json" ||
            mime=="application/xhtml+xml") {
            result["status"]=json!("non_text_content");
            break;
        }
        result["mime"]=json!(mime);
        match response_bytes(&mut response,MAX_PAGE_BYTES).await {
            Ok(bytes)=>{
                result["status"]=json!("fetched");
                result["excerpt"]=json!(excerpt(&bytes,mime=="text/html" || mime=="application/xhtml+xml"));
            }
            Err(_)=>result["status"]=json!("response_too_large_or_unreadable"),
        }
        break;
    }
    result
}

/// Return bounded best-effort page evidence. A slow page must not hold a
/// Queue batch (and other groups in that batch) indefinitely.
pub async fn evidence(msg: &Value) -> Vec<Value> {
    let mut results=Vec::new();
    for url in collect(msg) {
        if !safe_target(&url) {
            // Rejected hidden entity destinations are not moderation content.
            // Do not copy any part of their address into model evidence.
            results.push(json!({"status":"blocked_target"}));
            continue;
        }
        let displayed=public_url(&url);
        let controller=AbortController::default();
        let signal=controller.signal();
        // Bound redirects AND body streaming with one deadline per safe URL.
        // The per-message 2-link budget remains independent across groups.
        let fetched=fetch_page(url,displayed.clone(),&signal);
        let deadline=Delay::from(Duration::from_secs(MAX_PAGE_DURATION_SECS));
        match select(Box::pin(fetched),Box::pin(deadline)).await {
            Either::Left((result,_))=>results.push(result),
            Either::Right((_,_))=>{
                controller.abort();
                results.push(json!({"url":displayed,"status":"timeout"}));
            }
        };
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn excludes_local_and_credentialed_targets() {
        for target in ["http://127.0.0.1/", "https://[::1]/","https://example.local/",
            "https://u:p@example.org/","https://example.org:8080/","http://a.internal/",
            "https://localhost/","ftp://example.org/","https://169.254.169.254/"] {
            if let Some(url)=parse_url(target,false) {assert!(!safe_target(&url),"{target}");}
        }
        assert!(safe_target(&parse_url("https://example.org/a",false).unwrap()));
    }
    #[test]
    fn collects_raw_and_hidden_links_and_utf16_offsets() {
        let msg=json!({"text":"🔒 See https://a.example.org/x, and docs.example.org",
            "entities":[{"type":"url","offset":7,"length":23},
                        {"type":"text_link","url":"https://b.example.org/path","offset":36,"length":2}]});
        let links=collect(&msg);
        assert_eq!(links.len(),2);
        assert!(links.iter().any(|url|url.host_str()==Some("b.example.org")));
        assert!(links.iter().any(|url|url.host_str()==Some("a.example.org")));
        assert_eq!(utf16_span("🔒AB",2,2).as_deref(),Some("AB"));
    }
    #[test]
    fn never_exposes_query_secrets_as_page_urls() {
        let url=parse_url("https://example.org/path?token=secret#fragment",false).unwrap();
        assert_eq!(public_url(&url),"https://example.org/path");
    }
    #[test]
    fn hidden_credentials_are_collected_but_never_public() {
        for raw in ["https://hidden-user:HiddenPassword@example.org/path?token=secret#fragment",
            "https://hidden-user@example.org/path", "https://:HiddenPassword@example.org/path",
            "https://us%40er:pa%3Ass@example.org/path"] {
            let msg=json!({"text":"Read this", "entities":[
                {"type":"text_link","offset":0,"length":4,"url":raw}]});
            let urls=collect(&msg);
            assert_eq!(urls.len(),1);
            assert!(!safe_target(&urls[0]));
            assert_eq!(public_url(&urls[0]),"https://example.org/path");
        }
    }
    #[test]
    fn redirect_display_is_sanitized_without_weakening_target_validation() {
        let start=Url::parse("https://example.org/start").unwrap();
        let next=start.join("//redirect-user:RedirectPassword@example.org/end?token=secret#fragment").unwrap();
        assert!(!safe_target(&next));
        assert_eq!(public_url(&next),"https://example.org/end");
        let relative=start.join("/end?token=secret#fragment").unwrap();
        assert!(safe_target(&relative));
        assert_eq!(public_url(&relative),"https://example.org/end");
    }


    #[test]
    fn blocked_links_do_not_hide_later_safe_links() {
        let msg=json!({"text":
            "http://127.0.0.1/ https://u:p@example.org/private https://safe.example.org/article"});
        let urls=collect(&msg);
        assert!(urls.iter().any(|u|u.as_str()=="https://safe.example.org/article"),
            "rejected links must not exhaust the safe page-fetch budget");
    }
    #[test]
    fn hidden_rejected_entities_do_not_hide_safe_caption_urls() {
        let msg=json!({"caption":"Read https://safe.example.org/article",
            "caption_entities":[
                {"type":"text_link","offset":0,"length":4,"url":"http://127.0.0.1/"},
                {"type":"text_link","offset":0,"length":4,"url":"https://u:p@example.org/private"}
            ]});
        assert!(collect(&msg).iter()
            .any(|u|u.as_str()=="https://safe.example.org/article"));
    }
    #[test]
    fn continuous_rejected_links_and_duplicates_do_not_starve_safe_links() {
        let blocked=(0..220).map(|i|format!("http://127.0.0.1/{i}"))
            .collect::<Vec<_>>().join(" ");
        let msg=json!({"text":format!("{blocked} https://safe.example.org/article \
            https://safe.example.org/article https://second.example.org/news \
            https://third.example.org/ignored")});
        let urls=collect(&msg);
        assert_eq!(urls.iter().filter(|u|u.host_str()==Some("safe.example.org")).count(),1);
        assert!(urls.iter().any(|u|u.host_str()==Some("second.example.org")));
    }
    #[test]
    fn text_scan_stops_at_existing_token_limit() {
        let text=format!("{} https://safe.example.org/article",
            vec!["filler";256].join(" "));
        assert!(collect(&json!({"text":text})).is_empty(),
            "the link budget fix must not remove bounded scanning");
    }
}
