use axum::{body::Bytes, http::StatusCode};
use std::process::Stdio;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::oneshot,
    time::{timeout_at, Duration, Instant},
};

use crate::MAX_OUTPUT;

type Error = (StatusCode, &'static str);
const TOO_LARGE: Error = (StatusCode::PAYLOAD_TOO_LARGE, "transcoded file too large");
const UNSUPPORTED: Error = (StatusCode::UNPROCESSABLE_ENTITY, "unsupported audio format");
const PROCESS_FAILED: Error = (StatusCode::BAD_GATEWAY, "ffmpeg failed");

async fn read_output(mut stdout: impl AsyncRead + Unpin) -> Result<Vec<u8>, Error> {
    // Reserve once: Vec growth must not double the allocation at the boundary.
    let mut audio = Vec::with_capacity(MAX_OUTPUT);
    let mut chunk = [0; 8192];
    loop {
        // At the boundary, probe one byte to distinguish exactly MAX_OUTPUT from
        // an oversized stream. Never append bytes that would exceed the limit.
        let remaining = (MAX_OUTPUT - audio.len() + 1).min(chunk.len());
        let n = stdout
            .read(&mut chunk[..remaining])
            .await
            .map_err(|_| PROCESS_FAILED)?;
        if n == 0 {
            return Ok(audio);
        }
        if n > MAX_OUTPUT - audio.len() {
            return Err(TOO_LARGE);
        }
        audio.extend_from_slice(&chunk[..n]);
    }
}

pub(crate) async fn run(
    mut command: Command,
    bytes: Bytes,
    deadline: Duration,
) -> Result<Vec<u8>, Error> {
    // Use one absolute deadline, including spawn and supervisor scheduling time.
    let deadline = Instant::now() + deadline;
    let child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "ffmpeg is not installed"))?;

    // Dropping this sender cancels the supervisor, including when the request
    // future is dropped. The supervisor retains the Child so it can await reaping.
    let (cancel, cancelled) = oneshot::channel();
    let result = tokio::spawn(supervise(child, bytes, deadline, cancelled))
        .await
        .map_err(|_| PROCESS_FAILED)?;
    drop(cancel);
    result
}

#[cfg(test)]
#[path = "conversion_tests.rs"]
mod tests;

async fn supervise(
    mut child: Child,
    bytes: Bytes,
    deadline: Instant,
    mut cancelled: oneshot::Receiver<()>,
) -> Result<Vec<u8>, Error> {
    let result = {
        let work = async {
            let mut stdin = child.stdin.take().ok_or(PROCESS_FAILED)?;
            let stdout = child.stdout.take().ok_or(PROCESS_FAILED)?;
            let write = async move {
                stdin.write_all(&bytes).await.map_err(|_| UNSUPPORTED)?;
                // FFmpeg must receive EOF, even while stdout is still being read.
                drop(stdin);
                Ok(())
            };
            let wait = async {
                let status = child.wait().await.map_err(|_| PROCESS_FAILED)?;
                if status.success() {
                    Ok(())
                } else {
                    Err(UNSUPPORTED)
                }
            };
            // No detached stdin writer: all three futures are dropped together
            // on any error, timeout or cancellation, closing the pipe handles.
            let (_, audio, _) = tokio::try_join!(write, read_output(stdout), wait)?;
            if audio.is_empty() {
                return Err(TOO_LARGE);
            }
            Ok(audio)
        };
        tokio::select! {
            result = timeout_at(deadline, work) => result.unwrap_or(Err((
                StatusCode::GATEWAY_TIMEOUT, "audio conversion timeout",
            ))),
            _ = &mut cancelled => Err((StatusCode::BAD_GATEWAY, "conversion cancelled")),
        }
    };
    if result.is_err() {
        // Keep ownership until wait completes; kill_on_drop is a fallback, not
        // the normal cleanup path. wait is also safe if already reaped above.
        let _ = child.start_kill();
        child.wait().await.map_err(|_| PROCESS_FAILED)?;
    }
    result
}
