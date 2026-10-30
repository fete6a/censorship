//! Bounded, best-effort link evidence obtained with the Worker's native fetch.
//! A link/page is untrusted data, never a source of moderation instructions.
use std::collections::BTreeSet;
use std::net::IpAddr;
use serde_json::{json, Value};
use worker::{Fetch, Headers, Method, Request, RequestInit, RequestRedirect, Url};
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

// Keep both the decoded evidence and the scanned input bounded. The caller
// already caps fetched pages at 256 KiB, so this parser needs no DOM or regex.
fn html_entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let (digits, radix) = number.strip_prefix('x')
            .or_else(|| number.strip_prefix('X'))
            .map(|digits| (digits, 16))
            .unwrap_or((number, 10));
        let value = u32::from_str_radix(digits, radix).ok()?;
        return Some(char::from_u32(value).filter(|ch| *ch != '\0')
            .unwrap_or('\u{fffd}'));
    }
    Some(match name {
        "amp" | "AMP" => '&',
        "lt" | "LT" => '<',
        "gt" | "GT" => '>',
        "quot" | "QUOT" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "euro" => '€',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        _ => return None,
    })
}

/// Append Unicode characters without cutting a UTF-8 sequence at 2400 bytes.
fn append_excerpt(out: &mut String, space: &mut bool, input: &str, entities: bool) -> bool {
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        let ch = rest.chars().next().expect("nonempty UTF-8 suffix");
        let (ch, consumed) = if entities && ch == '&' {
            rest.as_bytes()[1..].iter().take(16).position(|b| *b == b';')
                .and_then(|end| html_entity(&rest[1..end + 1])
                    .map(|decoded| (decoded, end + 2)))
                .unwrap_or((ch, 1))
        } else {
            (ch, ch.len_utf8())
        };
        if out.len() >= MAX_EXCERPT { return false; }
        if ch.is_control() || ch.is_whitespace() {
            if !*space && !out.is_empty() {
                out.push(' ');
                *space = true;
            }
        } else {
            if out.len() + ch.len_utf8() > MAX_EXCERPT { return false; }
            out.push(ch);
            *space = false;
        }
        i += consumed;
    }
    true
}

/// Read a tag name and distinguish opening from closing tags. Stray '<'
/// characters in ordinary text (for example "price < 5") are not tags.
fn html_tag_name(input: &str, lt: usize) -> Option<(bool, &str, usize)> {
    let bytes = input.as_bytes();
    let mut i = lt + 1;
    let closing = bytes.get(i) == Some(&b'/');
    if closing { i += 1; }
    let first = i;
    if !bytes.get(i)?.is_ascii_alphabetic() { return None; }
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() ||
        matches!(bytes[i], b'-' | b':')) {
        i += 1;
    }
    Some((closing, &input[first..i], i))
}

/// A '>' within a quoted attribute is not a tag terminator.
fn html_tag_end(input: &str, from: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (offset, byte) in input.as_bytes()[from..].iter().enumerate() {
        match *byte {
            b'\'' | b'"' if quote == 0 => quote = *byte,
            b if quote != 0 && byte == &quote => quote = 0,
            b'>' if quote == 0 => return Some(from + offset + 1),
            _ => {}
        }
    }
    None
}

/// HTML accepts abrupt empty comments and the --!> terminator; XML does not.
fn html_comment_end(input: &str, from: usize, xml: bool) -> usize {
    if !xml {
        if input[from..].starts_with('>') { return from + 1; }
        if input[from..].starts_with("->") { return from + 2; }
    }
    let bytes = input.as_bytes();
    for i in from..bytes.len() {
        if bytes[i..].starts_with(b"-->") { return i + 3; }
        if !xml && bytes[i..].starts_with(b"--!>") { return i + 4; }
    }
    input.len()
}

fn self_closing(input: &str, after_name: usize, end: usize, name: &str, xml: bool) -> bool {
    // A slash on HTML non-void elements is ignored. SVG roots use foreign
    // content semantics; XML permits self-closing syntax on every element.
    (xml || name.eq_ignore_ascii_case("svg")) &&
        input[after_name..end - 1].trim_end().ends_with('/')
}

/// Scan ignored elements iteratively. A template creates its own delimiter
/// scope, so fake parent closings inside inert content cannot end the skip.
fn html_skipped_end<'a>(input: &'a str, mut from: usize, tag: &'a str, xml: bool) -> usize {
    let mut stack = vec![tag];
    while let Some(offset) = input[from..].find('<') {
        let lt = from + offset;
        let current = *stack.last().expect("ignored element scope");
        let raw = ["script", "style", "title", "iframe", "noscript"].iter()
            .any(|name| current.eq_ignore_ascii_case(name));
        if !raw && input[lt..].starts_with("<!--") {
            from = html_comment_end(input, lt + 4, xml);
            continue;
        }
        if let Some((closing, name, after_name)) = html_tag_name(input, lt) {
            let delimiter = input.as_bytes().get(after_name).is_some_and(|byte|
                *byte == b'>' || *byte == b'/' || byte.is_ascii_whitespace());
            if delimiter && (!raw || closing && name.eq_ignore_ascii_case(current)) {
                let Some(end) = html_tag_end(input, after_name) else { return input.len(); };
                if closing && name.eq_ignore_ascii_case(current) {
                    stack.pop();
                    if stack.is_empty() { return end; }
                } else if !closing && !raw {
                    if stack.len() == 1 && current.eq_ignore_ascii_case("head") &&
                        name.eq_ignore_ascii_case("body") { return lt; }
                    if !self_closing(input, after_name, end, name, xml) &&
                        (name.eq_ignore_ascii_case(current) ||
                         ["template", "script", "style", "title", "iframe", "noscript"].iter()
                            .any(|nested| name.eq_ignore_ascii_case(nested))) {
                        stack.push(name);
                    }
                }
                from = end;
                continue;
            }
        }
        from = lt + 1;
    }
    input.len()
}
fn excerpt(bytes: &[u8], mime: &str) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut space = false;
    let xml = mime == "application/xhtml+xml";
    if mime != "text/html" && !xml {
        append_excerpt(&mut out, &mut space, &text, false);
        return out.trim().to_owned();
    }

    let mut i = 0;
    while i < text.len() && out.len() < MAX_EXCERPT {
        let Some(next) = text[i..].find('<') else {
            append_excerpt(&mut out, &mut space, &text[i..], true);
            break;
        };
        let lt = i + next;
        if !append_excerpt(&mut out, &mut space, &text[i..lt], true) { break; }

        if text[lt..].starts_with("<!--") {
            // Unterminated comments never leak their payload into evidence.
            i = html_comment_end(&text, lt + 4, xml);
        } else if xml && text[lt..].starts_with("<![CDATA[") {
            let Some(end) = text[lt + 9..].find("]]>") else { break; };
            let raw = &text[lt + 9..lt + 9 + end];
            if !append_excerpt(&mut out, &mut space, raw, false) { break; }
            i = lt + 9 + end + 3;
        } else if text[lt..].starts_with("<!") || text[lt..].starts_with("<?") {
            let Some(end) = html_tag_end(&text, lt + 2) else { break; };
            i = end;
        } else if let Some((closing, name, after_name)) = html_tag_name(&text, lt) {
            let Some(end) = html_tag_end(&text, after_name) else { break; };
            let self_closing = self_closing(&text, after_name, end, name, xml);
            if !closing && !self_closing && [
                "head", "title", "script", "style", "noscript", "template",
                "nav", "aside", "footer", "svg", "canvas", "iframe", "object",
            ].iter().any(|ignored| name.eq_ignore_ascii_case(ignored)) {
                i = html_skipped_end(&text, end, name, xml);
            } else {
                i = end;
            }
        } else {
            // '<' by itself is ordinary text, not the start of a tag.
            if !append_excerpt(&mut out, &mut space, "<", false) { break; }
            i = lt + 1;
            continue;
        }
        // Preserve a word boundary between adjacent HTML elements.
        if !append_excerpt(&mut out, &mut space, " ", false) { break; }
    }
    out.trim().to_owned()
}

/// Return a JSON array of bounded page evidence. Network errors and
/// blocked/unsupported URLs cannot prevent reviewing the original message.
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
        let mut current=url;
        let mut result=json!({"url":displayed,"status":"unavailable"});
        for hop in 0..=MAX_REDIRECTS {
            let headers=Headers::new();
            if headers.set("Accept","text/html,text/plain,application/json;q=0.8").is_err() ||
                headers.set("Range","bytes=0-262143").is_err() {break;}
            let mut init=RequestInit::new();
            init.with_method(Method::Get).with_headers(headers).with_redirect(RequestRedirect::Manual);
            let Ok(request)=Request::new_with_init(current.as_str(),&init) else {break;};
            let Ok(mut response)=Fetch::Request(request).send().await else {break;};
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
                    result["excerpt"]=json!(excerpt(&bytes,&mime));
                }
                Err(_)=>result["status"]=json!("response_too_large_or_unreadable"),
            }
            break;
        }
        results.push(result);
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

    #[test]
    fn html_body_skips_large_scripts_styles_and_auxiliary_elements() {
        let html = format!(
            "<!doctype html><html><head><title>HEAD_POISON</title>\
            <style>{}</style><script>{}</script></head><body>\
            <nav>MENU_POISON</nav><template>TEMPLATE_POISON</template>\
            <main><h1>Useful &amp; verified</h1><p>News &#x1f30d; today.</p></main>\
            <footer>FOOTER_POISON</footer></body></html>",
            "CSS_POISON { display: none; }".repeat(500),
            "if (x < y) { JAVASCRIPT_POISON(); }".repeat(500),
        );
        assert_eq!(excerpt(html.as_bytes(), "text/html"), "Useful & verified News 🌍 today.");
    }
    #[test]
    fn html_comments_attributes_and_unclosed_tags_do_not_leak_markup() {
        let html = "<!-- HEADER_POISON > --><DIV data-title=\"a > b\">Alpha&nbsp; &amp; beta\
            <p>Second &#39;quoted&#39; line</p><broken";
        assert_eq!(excerpt(html.as_bytes(), "text/html"), "Alpha & beta Second 'quoted' line");
        assert_eq!(excerpt(b"<script>evil <p>bad</p>", "text/html"), "");
        assert_eq!(excerpt(b"<!-- unterminated", "text/html"), "");
        assert_eq!(excerpt(b"Price < 5 &gt; 3", "text/html"), "Price < 5 > 3");
        assert_eq!(excerpt(b"<p>&unknown; &#0; &#x110000;</p>", "text/html"),
            "&unknown; \u{fffd} \u{fffd}");
    }
    #[test]
    fn html_handles_case_insensitive_raw_tags_and_xhtml() {
        let html = "<STYLE type='text/css'>CSS_POISON</StYlE ><SCRIPT>JS_POISON</SCRIPT>\
            <main>Visible&nbsp; body<br/>paragraph &lt;tag&gt;</main>";
        assert_eq!(excerpt(html.as_bytes(), "text/html"), "Visible body paragraph <tag>");
    }
    #[test]
    fn excerpts_respect_utf8_byte_budget_and_plain_text_is_unchanged() {
        let many = "🌍".repeat(700);
        let html = format!("<main>{many}</main>");
        assert_eq!(excerpt(html.as_bytes(), "text/html"), "🌍".repeat(MAX_EXCERPT / 4));
        assert_eq!(excerpt(b"Plain <tag> &amp; text\nnext", "text/plain"),
            "Plain <tag> &amp; text next");
    }

    #[test]
    fn skipped_head_ignores_fake_closings_and_recovers_missing_end_tag() {
        let adversarial = "<head><script>let fake = '</head>';</script>\
            <!-- another </head> fake --><title>private metadata</title></head>\
            <body><main>Actual article</main></body>";
        assert_eq!(excerpt(adversarial.as_bytes(), "text/html"), "Actual article");
        let malformed = "<head><title>Hidden metadata</title><body><main>Readable body</main>";
        assert_eq!(excerpt(malformed.as_bytes(), "text/html"), "Readable body");
        let nested = "<nav><nav>Menu</nav>Still a menu</nav><main>Real content</main>";
        assert_eq!(excerpt(nested.as_bytes(), "text/html"), "Real content");
    }

    #[test]
    fn html_non_void_slashes_and_nested_templates_do_not_leak() {
        for html in [
            "<script/>POISON</script><style/>POISON</style><main>Article</main>",
            "<head/><script/>let x = '</head>';</script><title>POISON</title></head><main>Article</main>",
            "<nav><template></nav>POISON<template>POISON</template></template></nav><main>Article</main>",
        ] {
            assert_eq!(excerpt(html.as_bytes(), "text/html"), "Article");
        }
        let deep = format!("<nav>{}</nav>POISON{}</nav><main>Article</main>",
            "<template>".repeat(5000), "</template>".repeat(5000));
        assert_eq!(excerpt(deep.as_bytes(), "text/html"), "Article");
        assert_eq!(excerpt(b"<script/><style/><main>Article</main>", "application/xhtml+xml"), "Article");
        assert_eq!(excerpt(b"<svg/><main>Article</main>", "text/html"), "Article");
    }

    #[test]
    fn html_comment_recovery_preserves_article_and_ignored_scopes() {
        for comment in ["<!-->", "<!--->", "<!-- hidden --!>", "<!-- hidden -->"] {
            let html = format!("{comment}<main>Article</main>");
            assert_eq!(excerpt(html.as_bytes(), "text/html"), "Article");
            let nested = format!("<nav>{comment}POISON</nav><main>Article</main>");
            assert_eq!(excerpt(nested.as_bytes(), "text/html"), "Article");
        }
        assert_eq!(excerpt(b"<!--><main>Article</main>", "application/xhtml+xml"), "");
        assert_eq!(excerpt(b"<![CDATA[Article <text>]]>", "application/xhtml+xml"), "Article <text>");
    }

}
