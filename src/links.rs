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

fn self_closing(input: &str, after_name: usize, end: usize, name: &str,
                xml: bool, svg_foreign: bool) -> bool {
    // In text/html, a trailing slash is ignored on HTML non-void elements,
    // but honored in SVG foreign content, including its descendants.
    (xml || svg_foreign || name.eq_ignore_ascii_case("svg")) &&
        input[after_name..end - 1].trim_end().ends_with('/')
}

/// Scan ignored elements iteratively. Template and SVG nodes keep delimiter
/// scopes separate, so descendants cannot close their ignored parent.
fn html_skipped_end<'a>(input: &'a str, mut from: usize, tag: &'a str, xml: bool) -> usize {
    let mut stack = vec![tag];
    // Track foreign-content mode alongside ignored scopes in O(1) per tag.
    // A foreignObject switches to HTML until a nested svg starts again.
    let mut svg_modes = vec![tag.eq_ignore_ascii_case("svg")];
    while let Some(offset) = input[from..].find('<') {
        let lt = from + offset;
        let current = *stack.last().expect("ignored element scope");
        let raw = ["script", "style", "title", "iframe", "noscript"].iter()
            .any(|name| current.eq_ignore_ascii_case(name));
        let svg_foreign = *svg_modes.last().expect("SVG mode scope");
        if !xml && stack.len() == 1 && current.eq_ignore_ascii_case("head") &&
            input[from..lt].chars().any(|ch| !ch.is_whitespace()) {
            // Text also implicitly ends an HTML head when </head> is omitted.
            return from;
        }
        if !raw && input[lt..].starts_with("<!--") {
            from = html_comment_end(input, lt + 4, xml);
            continue;
        }
        if (xml || svg_foreign) && input[lt..].starts_with("<![CDATA[") {
            // The apparent </script> inside SVG CDATA is not an end tag.
            let Some(end) = input[lt + 9..].find("]]>") else { return input.len(); };
            from = lt + 9 + end + 3;
            continue;
        }
        if let Some((closing, name, after_name)) = html_tag_name(input, lt) {
            let delimiter = input.as_bytes().get(after_name).is_some_and(|byte|
                *byte == b'>' || *byte == b'/' || byte.is_ascii_whitespace());
            if delimiter && (!raw || closing && name.eq_ignore_ascii_case(current)) {
                let Some(end) = html_tag_end(input, after_name) else { return input.len(); };
                if closing && name.eq_ignore_ascii_case(current) {
                    stack.pop();
                    svg_modes.pop();
                    if stack.is_empty() { return end; }
                } else if !closing && !raw {
                    if !xml && stack.len() == 1 && current.eq_ignore_ascii_case("head") &&
                        !["html", "base", "basefont", "bgsound", "link", "meta",
                          "noframes", "script", "style", "template", "title", "noscript"]
                            .iter().any(|allowed| name.eq_ignore_ascii_case(allowed)) {
                        // HTML's "in head" insertion mode reprocesses a
                        // body-starting element after implicitly closing head.
                        return lt;
                    }
                    if !self_closing(input, after_name, end, name, xml, svg_foreign) &&
                        (name.eq_ignore_ascii_case(current) ||
                         ["template", "script", "style", "title", "iframe", "noscript", "svg"]
                            .iter().any(|nested| name.eq_ignore_ascii_case(nested)) ||
                         svg_foreign && name.eq_ignore_ascii_case("foreignobject")) {
                        let next_svg = if name.eq_ignore_ascii_case("svg") {
                            true
                        } else if svg_foreign && name.eq_ignore_ascii_case("foreignobject") {
                            false
                        } else {
                            svg_foreign
                        };
                        stack.push(name);
                        svg_modes.push(next_svg);
                    }
                }
                from = end;
                continue;
            }
        }
        from = lt + 1;
    }
    // No further markup: non-whitespace text after omitted </head> starts body.
    if !xml && stack.len() == 1 && stack[0].eq_ignore_ascii_case("head") &&
        input[from..].chars().any(|ch| !ch.is_whitespace()) {
        return from;
    }
    input.len()
}
// XHTML is XML, not HTML: namespaces are scoped, names are case-sensitive,
// CDATA is opaque, and every element can be self-closing. Track only open
// element names and xmlns attributes; no DOM or unbounded parser lookahead.
fn xml_tag_name(input: &str, lt: usize) -> Option<(bool, &str, usize)> {
    let mut i = lt + 1;
    let closing = input.as_bytes().get(i) == Some(&b'/');
    if closing { i += 1; }
    let mut chars = input[i..].char_indices();
    let (_, first) = chars.next()?;
    if first != '_' && !first.is_alphabetic() { return None; }
    let mut end = i + first.len_utf8();
    for (offset, ch) in chars {
        if ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':') {
            end = i + offset + ch.len_utf8();
        } else {
            break;
        }
    }
    Some((closing, &input[i..end], end))
}

struct XmlBinding<'a> {
    prefix: &'a str,
    uri: &'a str,
    depth: usize,
}

/// Collect only namespace declarations from a single opening tag, bounded by
/// the already located tag end. The most recent binding shadows an ancestor.
fn xml_namespace_attrs<'a>(input: &'a str, mut i: usize, end: usize,
                            depth: usize, bindings: &mut Vec<XmlBinding<'a>>) {
    let bytes = input.as_bytes();
    while i < end - 1 {
        while i < end - 1 && bytes[i].is_ascii_whitespace() { i += 1; }
        if i >= end - 1 || bytes[i] == b'/' { break; }
        let start = i;
        while i < end - 1 && !bytes[i].is_ascii_whitespace() &&
            !matches!(bytes[i], b'=' | b'/' | b'>') { i += 1; }
        if i == start { i += 1; continue; }
        let attr = &input[start..i];
        while i < end - 1 && bytes[i].is_ascii_whitespace() { i += 1; }
        if i >= end - 1 || bytes[i] != b'=' { continue; }
        i += 1;
        while i < end - 1 && bytes[i].is_ascii_whitespace() { i += 1; }
        if i >= end - 1 || !matches!(bytes[i], b'\'' | b'"') { continue; }
        let quote = bytes[i];
        i += 1;
        let value_start = i;
        while i < end - 1 && bytes[i] != quote { i += 1; }
        if i >= end - 1 { break; }
        let value = &input[value_start..i];
        i += 1;
        if attr == "xmlns" {
            bindings.push(XmlBinding {prefix: "", uri: value, depth});
        } else if let Some(prefix) = attr.strip_prefix("xmlns:") {
            bindings.push(XmlBinding {prefix, uri: value, depth});
        }
    }
}

// XML namespace values may use character references, including numeric ones.
// Compare against fixed known namespace URIs without allocating decoded copies.
fn xml_namespace_eq(raw: &str, expected: &str) -> bool {
    let mut chars = expected.chars();
    let mut i = 0;
    while i < raw.len() {
        let rest = &raw[i..];
        let first = rest.chars().next().expect("nonempty XML namespace");
        let (ch, consumed) = if first == '&' {
            let Some(end) = rest.as_bytes()[1..].iter().take(16).position(|b| *b == b';')
                else { return false; };
            let Some(decoded) = html_entity(&rest[1..end + 1]) else { return false; };
            (decoded, end + 2)
        } else {
            (first, first.len_utf8())
        };
        if chars.next() != Some(ch) { return false; }
        i += consumed;
    }
    chars.next().is_none()
}

fn xml_excerpt(text: &str) -> String {
    const XHTML: &str = "http://www.w3.org/1999/xhtml";
    const SVG: &str = "http://www.w3.org/2000/svg";
    struct Frame<'a> {
        name: &'a str,
        skip: bool,
    }
    let mut out = String::new();
    let mut space = false;
    let mut frames: Vec<Frame<'_>> = Vec::new();
    let mut bindings = Vec::new();
    let mut i = 0;
    while i < text.len() && out.len() < MAX_EXCERPT {
        let skipping = frames.last().is_some_and(|frame| frame.skip);
        let Some(offset) = text[i..].find('<') else {
            if !skipping { append_excerpt(&mut out, &mut space, &text[i..], true); }
            break;
        };
        let lt = i + offset;
        if !skipping && !append_excerpt(&mut out, &mut space, &text[i..lt], true) {
            break;
        }

        if text[lt..].starts_with("<!--") {
            i = html_comment_end(text, lt + 4, true);
        } else if text[lt..].starts_with("<![CDATA[") {
            let Some(end) = text[lt + 9..].find("]]>") else { break; };
            if !skipping && !append_excerpt(&mut out, &mut space,
                                            &text[lt + 9..lt + 9 + end], false) { break; }
            i = lt + 9 + end + 3;
        } else if text[lt..].starts_with("<?") {
            let Some(end) = text[lt + 2..].find("?>") else { break; };
            i = lt + 2 + end + 2;
        } else if text[lt..].starts_with("<!") {
            let Some(end) = html_tag_end(text, lt + 2) else { break; };
            i = end;
        } else if let Some((closing, name, after_name)) = xml_tag_name(text, lt) {
            let Some(end) = html_tag_end(text, after_name) else { break; };
            if closing {
                // Valid XML closes the innermost element. Do not scan the
                // entire stack for malformed end tags (quadratic nesting).
                if frames.last().is_some_and(|frame| frame.name == name) {
                    frames.pop();
                    while bindings.last().is_some_and(|binding: &XmlBinding<'_>|
                        binding.depth > frames.len()) { bindings.pop(); }
                }
            } else {
                let depth = frames.len() + 1;
                xml_namespace_attrs(text, after_name, end, depth, &mut bindings);
                let (prefix, local) = name.rsplit_once(':').unwrap_or(("", name));
                let uri = bindings.iter().rev()
                    .find(|binding| binding.prefix == prefix)
                    .map(|binding| binding.uri);
                // For compatibility with XHTML fragments without xmlns, bare
                // HTML names are treated as XHTML when no default is declared.
                let xhtml = uri.is_some_and(|uri| xml_namespace_eq(uri, XHTML)) ||
                    (uri.is_none() && prefix.is_empty());
                let svg = uri.is_some_and(|uri| xml_namespace_eq(uri, SVG)) ||
                    (uri.is_none() && prefix.is_empty() && local == "svg");
                let ignored = (xhtml && [
                    "head", "title", "script", "style", "noscript", "template",
                    "nav", "aside", "footer", "canvas", "iframe", "object",
                ].contains(&local)) || (svg && local == "svg");
                let self_closed = text[after_name..end - 1].trim_end().ends_with('/');
                if self_closed {
                    while bindings.last().is_some_and(|binding: &XmlBinding<'_>|
                        binding.depth >= depth) { bindings.pop(); }
                } else {
                    frames.push(Frame {name, skip: skipping || ignored});
                }
            }
            i = end;
        } else {
            // Invalid XML syntax is ordinary text outside ignored elements.
            if !skipping && !append_excerpt(&mut out, &mut space, "<", false) { break; }
            i = lt + 1;
            continue;
        }
        if !frames.last().is_some_and(|frame| frame.skip) {
            if !append_excerpt(&mut out, &mut space, " ", false) { break; }
        }
    }
    out.trim().to_owned()
}

fn excerpt(bytes: &[u8], mime: &str) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut space = false;
    let xml = mime == "application/xhtml+xml";
    if xml { return xml_excerpt(&text); }
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
            let self_closing = self_closing(&text, after_name, end, name, xml, false);
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
                result["excerpt"]=json!(excerpt(&bytes,&mime));
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


    #[test]
    fn html_recovers_implicit_body_after_head_without_explicit_end_tags() {
        for html in [
            "<head><title>HEAD_POISON</title><main>Article</main>",
            "<head><meta name='description' content='HEAD_POISON'><div>Article</div>",
            "<head><script>const x = '</head>';</script><!-- fake <body> -->\
             <nav>MENU_POISON</nav><main>Article</main>",
            "<head><title>HEAD_POISON</title>Article",
        ] {
            assert_eq!(excerpt(html.as_bytes(), "text/html"), "Article", "{html}");
        }
    }

    #[test]
    fn svg_foreign_self_closing_elements_do_not_swallow_later_body() {
        for html in [
            "<svg><script/></svg><main>Article</main>",
            "<svg><style/><title/></svg><main>Article</main>",
            "<nav><svg><script/><title/></svg>MENU_POISON</nav><main>Article</main>",
            // foreignObject is an HTML integration point: script/ is not void there.
            "<svg><foreignObject><script/>INERT_POISON</script></foreignObject>\
             </svg><main>Article</main>",
        ] {
            assert_eq!(excerpt(html.as_bytes(), "text/html"), "Article", "{html}");
        }
    }

    #[test]
    fn xhtml_cdata_is_opaque_in_scripts_styles_and_nested_skips() {
        for html in [
            "<script><![CDATA[const x='</script>'; CDATA_POISON;]]></script><main>Article</main>",
            "<style><![CDATA[.x{content:'</style>'} CSS_POISON]]></style><main>Article</main>",
            "<nav><script><![CDATA[const x='</script>'; CDATA_POISON]]></script>\
             MENU_POISON</nav><main>Article</main>",
        ] {
            assert_eq!(excerpt(html.as_bytes(), "application/xhtml+xml"), "Article", "{html}");
        }
        assert_eq!(excerpt(b"<script><![CDATA[const x='</script>'; POISON",
            "application/xhtml+xml"), "");
    }

    #[test]
    fn xhtml_namespace_bindings_match_local_names_and_obey_rebinding() {
        let ns = "http://www.w3.org/1999/xhtml";
        let html = format!(
            "<h:html xmlns:h='{ns}'><h:script>{}</h:script><h:main>Article</h:main></h:html>",
            "SCRIPT_POISON ".repeat(500)
        );
        assert_eq!(excerpt(html.as_bytes(), "application/xhtml+xml"), "Article");
        let local = format!(
            "<h:script xmlns:h='{ns}'>{}</h:script><main>Article</main>",
            "SCRIPT_POISON ".repeat(500)
        );
        assert_eq!(excerpt(local.as_bytes(), "application/xhtml+xml"), "Article");
        // XML character references in xmlns must not bypass script filtering.
        let encoded = format!(
            "<h:script xmlns:h='http://www.w3.org/1999/x&#104;tml'>{}</h:script>\
             <main>Article</main>",
            "SCRIPT_POISON ".repeat(500)
        );
        assert_eq!(excerpt(encoded.as_bytes(), "application/xhtml+xml"), "Article");
        let nested = format!(
            "<h:html xmlns:h='{ns}'><h:nav><h:template><h:nav>POISON</h:nav></h:template>\
             MENU_POISON</h:nav><h:main>Article</h:main></h:html>"
        );
        assert_eq!(excerpt(nested.as_bytes(), "application/xhtml+xml"), "Article");
        let rebound = format!(
            "<h:html xmlns:h='{ns}'><h:div xmlns:h='urn:foreign'>\
             <h:script>Foreign visible</h:script></h:div>\
             <h:script>SCRIPT_POISON</h:script><h:main>Article</h:main></h:html>"
        );
        assert_eq!(excerpt(rebound.as_bytes(), "application/xhtml+xml"),
            "Foreign visible Article");
        assert_eq!(excerpt(
            b"<f:script xmlns:f='urn:foreign'>Visible</f:script><main>Article</main>",
            "application/xhtml+xml"), "Visible Article");
    }

    #[test]
    fn parser_review_boundaries_remain_bounded_and_keep_plain_text() {
        let html = format!(
            "<head><title>HEAD_POISON</title><main>{}</main>",
            "🌍".repeat(700)
        );
        assert_eq!(excerpt(html.as_bytes(), "text/html"),
            "🌍".repeat(MAX_EXCERPT / 4));
        assert_eq!(excerpt(b"<script>not markup</script> Article", "text/plain"),
            "<script>not markup</script> Article");
    }

}
