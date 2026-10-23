use axum::{
    body::Bytes,
    extract::DefaultBodyLimit,
    http::{header, HeaderMap, StatusCode},
    routing::post,
    Router,
};
use std::process::Stdio;
use tokio::{io::AsyncWriteExt, process::Command, time::{timeout, Duration}};
use subtle::ConstantTimeEq;

const MAX_INPUT: usize = 8 * 1024 * 1024;
const MAX_OUTPUT: usize = 8 * 1024 * 1024;

async fn transcode(headers: HeaderMap, bytes: Bytes) -> Result<([(header::HeaderName, &'static str); 1], Vec<u8>), (StatusCode, &'static str)> {
    let expected = std::env::var("TRANSCODER_TOKEN")
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "missing server token"))?;
    if expected.len() < 24 { return Err((StatusCode::INTERNAL_SERVER_ERROR, "weak token")); }
    let got = headers.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .strip_prefix("Bearer ")
        .unwrap_or("");
    let authorized = got.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 1;
    if !authorized { return Err((StatusCode::UNAUTHORIZED, "unauthorized")); }
    if bytes.is_empty() || bytes.len() > MAX_INPUT {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "unsupported input size"));
    }
    let task = async {
        let mut process = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i", "pipe:0",
                   "-ac", "1", "-ar", "16000", "-b:a", "64k", "-f", "mp3", "pipe:1"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "ffmpeg is not installed"))?;
        let mut input = process.stdin.take().ok_or((StatusCode::INTERNAL_SERVER_ERROR,"stdin"))?;
        let writer = tokio::spawn(async move {
            input.write_all(&bytes).await
        });
        let out = process.wait_with_output().await
            .map_err(|_| (StatusCode::BAD_GATEWAY, "ffmpeg failed"))?;
        let write_result = writer.await
            .map_err(|_| (StatusCode::BAD_GATEWAY, "stdin failed"))?;
        if write_result.is_err() || !out.status.success() {
            return Err((StatusCode::UNPROCESSABLE_ENTITY, "unsupported audio format"));
        }
        if out.stdout.len() > MAX_OUTPUT || out.stdout.is_empty() {
            return Err((StatusCode::PAYLOAD_TOO_LARGE,"transcoded file too large"));
        }
        Ok::<_, (StatusCode, &'static str)>(out.stdout)
    };
    let audio = timeout(Duration::from_secs(25), task).await
        .map_err(|_| (StatusCode::GATEWAY_TIMEOUT, "audio conversion timeout"))??;
    Ok(([(header::CONTENT_TYPE, "audio/mpeg")], audio))
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/transcode", post(transcode))
        .layer(DefaultBodyLimit::max(MAX_INPUT));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8788").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
