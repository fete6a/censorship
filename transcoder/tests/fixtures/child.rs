//! Standalone deterministic subprocess for pipe and lifecycle regression tests.
use std::io::{Read, Write};

fn emit(size: usize) {
    let chunk = [b'M'; 8192];
    let mut stdout = std::io::stdout().lock();
    let mut remaining = size;
    while remaining > 0 {
        let n = remaining.min(chunk.len());
        stdout.write_all(&chunk[..n]).unwrap();
        remaining -= n;
    }
    stdout.flush().unwrap();
}

fn drain() -> Vec<u8> {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    input
}

fn hang() {
    std::thread::sleep(std::time::Duration::from_secs(60));
}

fn main() {
    if let Some(path) = std::env::var_os("TRANSCODER_TEST_PID_FILE") {
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap();
    let size: usize = args.next().unwrap().parse().unwrap();
    match mode.as_str() {
        "echo" => std::io::stdout().write_all(&drain()).unwrap(),
        "size" => {
            drain();
            emit(size);
        }
        "output-first" => {
            emit(size);
            assert_eq!(drain().len(), 8 * 1024 * 1024);
        }
        "flood" => {
            emit(size);
            hang();
        }
        "fail" => std::process::exit(7),
        "hang" => hang(),
        // Close only the selected pipe, keeping the process alive. The output
        // fixture is also useful on non-Unix platforms via the other modes.
        #[cfg(unix)]
        "close-input" => {
            use std::os::fd::FromRawFd;
            drop(unsafe { std::fs::File::from_raw_fd(0) });
            hang();
        }
        #[cfg(unix)]
        "close-output" => {
            use std::os::fd::FromRawFd;
            drain();
            drop(unsafe { std::fs::File::from_raw_fd(1) });
            hang();
        }
        _ => panic!("unknown fixture mode"),
    }
}
