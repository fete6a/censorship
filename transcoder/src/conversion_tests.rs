use super::*;
use std::{path::PathBuf, sync::OnceLock};
#[cfg(unix)]
use tokio::time::sleep;
use tokio::time::timeout;

const TEST_DEADLINE: Duration = Duration::from_secs(5);

// A standalone Rust child gives deterministic pipe/exit behavior without
// FFmpeg, real audio, a shell, or any subprocess descendants.
fn fixture(mode: &str, size: usize) -> Command {
    static EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();
    let executable = EXECUTABLE.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = root.join(format!(
            "target/transcoder-test-child-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let status =
            std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .arg(root.join("tests/fixtures/child.rs"))
                .arg("--edition=2021")
                .arg("-o")
                .arg(&path)
                .status()
                .expect("compile synthetic child with rustc");
        assert!(status.success());
        path
    });
    let mut command = Command::new(executable);
    command.args([mode, &size.to_string()]);
    command
}

async fn convert(mode: &str, size: usize, input: Bytes) -> Result<Vec<u8>, Error> {
    timeout(
        TEST_DEADLINE,
        run(fixture(mode, size), input, TEST_DEADLINE),
    )
    .await
    .expect("conversion deadlocked")
}

#[tokio::test]
async fn normal_output_and_stdin_eof() {
    let audio = convert("echo", 0, Bytes::from_static(b"synthetic audio"))
        .await
        .unwrap();
    assert_eq!(audio, b"synthetic audio");
}

#[tokio::test]
async fn exact_limit_is_accepted_without_vec_growth() {
    let audio = convert("size", MAX_OUTPUT, Bytes::new()).await.unwrap();
    assert_eq!(audio.len(), MAX_OUTPUT);
    assert_eq!(audio.capacity(), MAX_OUTPUT);
    assert!(audio.iter().all(|&b| b == b'M'));
}

#[tokio::test]
async fn one_byte_over_limit_is_rejected() {
    assert_eq!(
        convert("size", MAX_OUTPUT + 1, Bytes::new()).await,
        Err(TOO_LARGE)
    );
}

#[tokio::test]
async fn empty_output_is_rejected() {
    assert_eq!(convert("size", 0, Bytes::new()).await, Err(TOO_LARGE));
}

#[tokio::test]
async fn stdout_is_drained_while_stdin_is_blocked() {
    // Both pipes exceed their kernel buffers. Serial write-then-read deadlocks.
    let audio = convert(
        "output-first",
        MAX_OUTPUT,
        Bytes::from(vec![b'I'; crate::MAX_INPUT]),
    )
    .await
    .unwrap();
    assert_eq!(audio.len(), MAX_OUTPUT);
}

#[tokio::test]
async fn abnormal_exit_interrupts_blocked_stdin() {
    assert_eq!(
        convert("fail", 0, Bytes::from(vec![0; crate::MAX_INPUT])).await,
        Err(UNSUPPORTED)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stdin_failure_terminates_child_that_keeps_running() {
    assert_eq!(
        convert("close-input", 0, Bytes::from(vec![0; crate::MAX_INPUT])).await,
        Err(UNSUPPORTED)
    );
}

#[tokio::test]
async fn stdout_read_failure_is_reported() {
    struct FailedRead;
    impl AsyncRead for FailedRead {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Err(std::io::Error::other("synthetic read failure")))
        }
    }
    assert_eq!(read_output(FailedRead).await, Err(PROCESS_FAILED));
}

#[tokio::test]
async fn missing_executable_is_reported() {
    let command = Command::new("/nonexistent/transcoder-test-ffmpeg");
    assert_eq!(
        run(command, Bytes::new(), TEST_DEADLINE)
            .await
            .unwrap_err()
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[cfg(unix)]
fn child_exists(pid: u32) -> bool {
    // SAFETY: signal 0 only checks the existence of this fixture's PID.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        true
    } else {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        false
    }
}

#[cfg(unix)]
fn assert_reaped(pid: u32) {
    // SAFETY: this is our own fixture child; the status pointer is valid. A
    // running child returns 0, a zombie returns its PID. Only ECHILD proves
    // that the supervisor already waited for it (rather than just killing it).
    let mut status = 0;
    let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    assert_eq!(result, -1, "child was still running or a zombie");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

// Exercise the production supervisor while retaining the PID for OS-level
// cleanup assertions, independent of procfs availability or PID namespaces.
#[cfg(unix)]
async fn supervised(mode: &str, size: usize, deadline: Duration) -> (u32, Result<Vec<u8>, Error>) {
    let deadline = Instant::now() + deadline;
    let child = fixture(mode, size)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    assert!(child_exists(pid));
    let (cancel, cancelled) = oneshot::channel();
    let result = timeout(
        TEST_DEADLINE,
        supervise(
            child,
            Bytes::from(vec![0; crate::MAX_INPUT]),
            deadline,
            cancelled,
        ),
    )
    .await
    .expect("supervisor did not finish cleanup");
    drop(cancel);
    assert_reaped(pid);
    (pid, result)
}

#[cfg(unix)]
#[tokio::test]
async fn supervisor_does_not_restart_an_expired_deadline() {
    let child = fixture("hang", 0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let (cancel, cancelled) = oneshot::channel();
    let result = timeout(
        Duration::from_secs(1),
        supervise(
            child,
            Bytes::new(),
            Instant::now() - Duration::from_secs(1),
            cancelled,
        ),
    )
    .await
    .expect("expired deadline was restarted");
    drop(cancel);
    assert_eq!(result.unwrap_err().0, StatusCode::GATEWAY_TIMEOUT);
    assert_reaped(pid);
}

#[cfg(unix)]
#[tokio::test]
async fn overflow_kills_and_reaps_without_waiting_for_eof_or_stdin() {
    let (_, result) = supervised("flood", MAX_OUTPUT + 1, TEST_DEADLINE).await;
    assert_eq!(result, Err(TOO_LARGE));
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_and_reaps_with_stdin_blocked() {
    let (_, result) = supervised("hang", 0, Duration::from_millis(100)).await;
    assert_eq!(result.unwrap_err().0, StatusCode::GATEWAY_TIMEOUT);
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_still_applies_after_stdout_eof() {
    let (_, result) = supervised("close-output", 0, Duration::from_millis(100)).await;
    assert_eq!(result.unwrap_err().0, StatusCode::GATEWAY_TIMEOUT);
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_request_future_cancels_and_reaps_child() {
    let marker = std::env::temp_dir().join(format!("transcoder-cancel-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let mut command = fixture("hang", 0);
    command.env("TRANSCODER_TEST_PID_FILE", &marker);
    let request = tokio::spawn(run(
        command,
        Bytes::from(vec![0; crate::MAX_INPUT]),
        TEST_DEADLINE,
    ));
    let pid = timeout(TEST_DEADLINE, async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(&marker) {
                if let Ok(pid) = pid.parse::<u32>() {
                    break pid;
                }
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child never started");
    assert!(child_exists(pid));
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    timeout(Duration::from_secs(2), async {
        while child_exists(pid) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled request left a running child or zombie");
    assert_reaped(pid);
    std::fs::remove_file(marker).unwrap();
}

#[tokio::test]
#[ignore = "finite 32 MiB reproduction of the legacy buffering behavior"]
async fn legacy_wait_with_output_allocates_past_limit() {
    let child = fixture("size", 4 * MAX_OUTPUT)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let output = timeout(TEST_DEADLINE, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 4 * MAX_OUTPUT);
    assert!(output.stdout.capacity() > MAX_OUTPUT);
    eprintln!(
        "legacy: stdout length={}, capacity={}, limit={}",
        output.stdout.len(),
        output.stdout.capacity(),
        MAX_OUTPUT
    );
}
