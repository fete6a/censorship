# Audio transcoder resource limits

The Rust/Axum service accepts `POST /transcode` with the existing Bearer token
(`TRANSCODER_TOKEN`, at least 24 characters). Input remains limited to 8 MiB by
Axum and the handler. FFmpeg still produces mono, 16 kHz, 64 kbit/s MP3 with
`Content-Type: audio/mpeg`. The conversion deadline remains 25 seconds.

## Output buffering and process ownership

| Approach | Behavior at the 8 MiB output limit |
| --- | --- |
| Previous `wait_with_output()` | Collects all stdout into a growing `Vec` before checking its length. The allocation can exceed the limit, even before rejection. |
| Bounded streaming reader | Reserves an 8 MiB output `Vec` once, reads through an 8 KiB scratch buffer, and never appends past the limit. After exactly 8 MiB it probes one byte: EOF permits success; another byte returns HTTP 413 immediately. |

stdin writing, stdout reading and child waiting run concurrently. The writer
closes stdin after the complete input, so FFmpeg receives EOF. An I/O failure,
nonzero exit, overflow or timeout cancels the other I/O futures and closes their
pipes. The supervisor then kills and waits for the child before returning the
error. stderr remains discarded rather than buffered.

If the request future is dropped, its cancellation sender is dropped too. A
separate supervisor retains the child long enough to kill and reap it; there is
no detached stdin writer retaining the request body. `kill_on_drop(true)` remains
as a fallback. The 25-second deadline covers conversion I/O and process exit;
cleanup waits for reaping afterward. A client disconnect only triggers this path
if the server drops the handler future.

This bounds the service's **per-conversion stdout buffer**, with an additional
8 KiB scratch buffer and a one-byte overflow probe within that scratch buffer.
It is not a total service or FFmpeg memory cap: input bodies, runtime overhead,
allocator bookkeeping, FFmpeg's own memory and concurrent requests are separate.

## Local regression checks

```sh
cargo test --manifest-path transcoder/Cargo.toml
cargo clippy --manifest-path transcoder/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path transcoder/Cargo.toml -- --check
```

Tests compile a standalone, standard-library-only Rust subprocess fixture with
`rustc`. No FFmpeg, Python, network, production credentials or Telegram audio is
needed. Coverage includes normal and empty output, exactly 8 MiB, 8 MiB + 1 byte,
output before stdin consumption, broken stdin, stdout read failure, abnormal
exit and a missing executable. Unix tests also use `kill(pid, 0)` and
`waitpid(..., WNOHANG)`/`ECHILD` to check child termination and reaping, even
when stdin is blocked or stdout has already closed. Timeout tests inject a
100 ms deadline; the HTTP handler continues to supply 25 seconds. The `libc`
dependency is used only by these Unix tests and is not added to production code.

The finite legacy reproduction is opt-in:

```sh
cargo test --manifest-path transcoder/Cargo.toml \
  legacy_wait_with_output_allocates_past_limit -- --ignored --nocapture
```

On 2026-10-29, against main `f18b5d751c1c65afd239e48f36b4824982c00ff4`,
the original service accepted a synthetic subprocess's 32 MiB stdout and waited
for child exit before returning HTTP 413. The isolated `wait_with_output()`
reproduction with Tokio 1.54.1 returned stdout length **33,554,432** and `Vec`
capacity **67,108,864** bytes, against an 8,388,608-byte limit. Vec capacity is
allocation evidence, not a measurement of resident physical memory. This test
does not attempt OOM, and establishes no production outage or exploitation.
