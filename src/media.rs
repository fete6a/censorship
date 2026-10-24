use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;
use worker::{Env, Fetch, Headers, Method, Request, RequestInit, Result};
use crate::tg;
use crate::body::response_bytes;
use crate::util::{config, fail, field_int, field_str, secret};

const MAX_IMAGE: usize = 4 * 1024 * 1024;
const MAX_AUDIO: usize = 8 * 1024 * 1024;
const MAX_VIDEO: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub struct ModelMedia {
    pub images: Vec<String>,
    pub audio: Vec<String>,
    pub videos: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MediaKind {
    Image,
    Audio,
    Video,
}

struct SelectedMedia {
    file_id: String,
    kind: MediaKind,
    mime: String,
    size: Option<i64>,
    duration: Option<i64>,
}

fn choose(msg: &Value) -> Result<Option<SelectedMedia>> {
    let (obj, kind, assumed_mime): (&Value, MediaKind, &str) = if !msg["photo"].is_null() {
        let photos=msg["photo"].as_array().ok_or_else(||fail("Invalid photo metadata"))?;
        match photos.last() {
            Some(photo) => (photo, MediaKind::Image, "image/jpeg"),
            None => return Err(fail("Empty photo metadata")),
        }
    } else if !msg["video"].is_null() {
        (&msg["video"], MediaKind::Video, "video/mp4")
    } else if !msg["video_note"].is_null() {
        (&msg["video_note"], MediaKind::Video, "video/mp4")
    } else if !msg["animation"].is_null() {
        (&msg["animation"], MediaKind::Video, "video/mp4")
    } else if !msg["sticker"].is_null() {
        let sticker = &msg["sticker"];
        if sticker["is_animated"] == true {
            return Err(fail("Animated TGS sticker needs manual review"));
        }
        if sticker["is_video"] == true {
            (sticker, MediaKind::Video, "video/webm")
        } else {
            (sticker, MediaKind::Image, "image/webp")
        }
    } else if !msg["voice"].is_null() {
        (&msg["voice"], MediaKind::Audio, "audio/ogg")
    } else if !msg["audio"].is_null() {
        (&msg["audio"], MediaKind::Audio, "audio/unknown")
    } else if !msg["document"].is_null() {
        let document = &msg["document"];
        let mime = field_str(document, "mime_type").to_ascii_lowercase();
        let kind = if mime.starts_with("image/") {
            MediaKind::Image
        } else if mime.starts_with("audio/") {
            MediaKind::Audio
        } else if mime.starts_with("video/") {
            MediaKind::Video
        } else {
            return Err(fail("Unsupported document media; manual review required"));
        };
        (document, kind, "application/octet-stream")
    } else {
        return Ok(None);
    };

    let file_id = field_str(obj, "file_id");
    if file_id.is_empty() {
        return Err(fail("Telegram omitted the media file_id"));
    }
    let mime = obj.get("mime_type").and_then(Value::as_str).unwrap_or(assumed_mime).to_ascii_lowercase();
    Ok(Some(SelectedMedia {
        file_id: file_id.to_owned(),
        kind,
        mime,
        size: field_int(obj, "file_size"),
        duration: field_int(obj, "duration"),
    }))
}

async fn transcode_audio(env: &Env, bytes: Vec<u8>, input_mime: &str) -> Result<Vec<u8>> {
    let url = config(env, "TRANSCODER_URL");
    if url.is_empty() || !url.starts_with("https://") {
        return Err(fail("Voice/audio format needs an HTTPS TRANSCODER_URL for WAV/MP3 conversion"));
    }
    let token = secret(env, "TRANSCODER_TOKEN")?;
    let headers = Headers::new();
    headers.set("Content-Type", input_mime)?;
    headers.set("Authorization", &format!("Bearer {token}"))?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post);
    init.with_headers(headers);
    init.with_body(Some(js_sys::Uint8Array::from(bytes.as_slice()).into()));
    let request = Request::new_with_init(&url, &init)?;
    let mut response = Fetch::Request(request).send().await?;
    if response.status_code() != 200 {
        return Err(fail("Audio transcoder failed"));
    }
    if response.headers().get("Content-Type")?.unwrap_or_default().split(';').next() != Some("audio/mpeg") {
        return Err(fail("Audio transcoder must return audio/mpeg"));
    }
    let result = response_bytes(&mut response, MAX_AUDIO).await?;
    if result.is_empty() {
        return Err(fail("Transcoded audio is empty"));
    }
    Ok(result)
}

pub async fn for_message(env: &Env, msg: &Value) -> Result<ModelMedia> {
    let mut media = ModelMedia::default();
    let Some(meta) = choose(msg)? else { return Ok(media); };
    let max = match meta.kind {
        MediaKind::Image => MAX_IMAGE,
        MediaKind::Audio => MAX_AUDIO,
        MediaKind::Video => MAX_VIDEO,
    };
    if meta.size.is_some_and(|n| n < 0 || n as usize > max) {
        return Err(fail("Media is too large for Clef-omni"));
    }
    if meta.kind == MediaKind::Video && meta.duration.is_some_and(|n| n > 60) {
        return Err(fail("Video exceeds Clef-omni 60-second limit"));
    }
    if meta.kind == MediaKind::Audio && meta.duration.is_some_and(|n| n > 300) {
        return Err(fail("Audio exceeds Clef-omni 300-second limit"));
    }
    let mut mime = meta.mime;
    let mut bytes = tg::download(env, &meta.file_id, max).await?;
    if bytes.is_empty() {
        return Err(fail("Empty media file"));
    }
    match meta.kind {
        MediaKind::Image => {
            if !matches!(mime.as_str(), "image/jpeg" | "image/png" | "image/webp") {
                return Err(fail("Image format is unsupported"));
            }
            media.images.push(format!("data:{mime};base64,{}", STANDARD.encode(bytes)));
        }
        MediaKind::Video => {
            if !matches!(mime.as_str(), "video/mp4" | "video/webm") {
                return Err(fail("Video format is unsupported; use MP4/WebM"));
            }
            media.videos.push(format!("data:{mime};base64,{}", STANDARD.encode(bytes)));
        }
        MediaKind::Audio => {
            if !matches!(mime.as_str(), "audio/wav" | "audio/wave" | "audio/x-wav" | "audio/mpeg" | "audio/mp3") {
                bytes = transcode_audio(env, bytes, &mime).await?;
                mime = "audio/mpeg".into();
            } else if mime != "audio/mpeg" && mime != "audio/mp3" {
                mime = "audio/wav".into();
            } else {
                mime = "audio/mpeg".into();
            }
            media.audio.push(format!("data:{mime};base64,{}", STANDARD.encode(bytes)));
        }
    }
    Ok(media)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn telegram_voice_defaults_to_ogg_needing_transcode() {
        let v = json!({"voice":{"file_id":"abc", "file_size":2000,"duration":25}});
        let m = choose(&v).unwrap().unwrap();
        assert_eq!(m.kind, MediaKind::Audio);
        assert_eq!(m.mime, "audio/ogg");
    }
    #[test]
    fn document_is_not_unconditionally_treated_as_audio() {
        let v=json!({"document":{"file_id":"x","mime_type":"application/pdf"}});
        assert!(choose(&v).is_err());
    }
    #[test]
    fn static_sticker_is_image_and_video_sticker_is_video() {
        let a=json!({"sticker":{"file_id":"x","is_video":false,"is_animated":false}});
        let b=json!({"sticker":{"file_id":"y","is_video":true,"is_animated":false}});
        assert_eq!(choose(&a).unwrap().unwrap().mime,"image/webp");
        assert_eq!(choose(&b).unwrap().unwrap().mime,"video/webm");
    }
}
