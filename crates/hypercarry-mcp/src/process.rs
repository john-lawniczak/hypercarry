use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub fn hash(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    Sha256::digest(bytes)
        .iter()
        .flat_map(|b| {
            [
                char::from(HEX[usize::from(b >> 4)]),
                char::from(HEX[usize::from(b & 15)]),
            ]
        })
        .collect()
}
pub fn verify_binary(path: &Path, expected: &str) -> Result<()> {
    ensure!(
        path.is_absolute() && fs::symlink_metadata(path)?.file_type().is_file(),
        "binary must be an absolute regular file"
    );
    ensure!(
        hash(&fs::read(path)?) == expected,
        "binary SHA-256 mismatch"
    );
    Ok(())
}

/// No shell, inherited HYPERCARRY settings, caller-selected executable or stdin.
/// Read output concurrently to avoid pipe deadlock, cap it, and reap on timeout.
pub fn run(
    path: &Path,
    digest: &str,
    args: &[String],
    timeout_ms: u64,
    stop: &AtomicBool,
) -> Result<Value> {
    verify_binary(path, digest)?;
    ensure!(!stop.load(Ordering::Relaxed), "server is stopping");
    let mut child = Command::new(path)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("cannot start pinned command")?;
    let stdout = child.stdout.take().context("command stdout unavailable")?;
    let (send, receive) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = send.send(result);
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if stop.load(Ordering::Relaxed) || started.elapsed() >= Duration::from_millis(timeout_ms) {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "command interrupted or timed out; mutations may have completed, inspect state before retrying"
            );
        }
        thread::sleep(Duration::from_millis(10));
    };
    ensure!(
        status.success(),
        "command failed with exit code {:?}; no successful result is claimed",
        status.code()
    );
    let bytes = receive
        .recv_timeout(Duration::from_millis(100))
        .context("command output incomplete")??;
    ensure!(bytes.len() <= 1_048_576, "command output exceeds 1 MiB");
    let value: Value = serde_json::from_slice(&bytes).context("command output is not JSON")?;
    // Callers annotate this result with provenance keys, which is only defined
    // for an object. An array or scalar would otherwise panic *after* the pinned
    // command has run, discarding the report of an already-completed mutation.
    ensure!(
        value.is_object(),
        "command output is not a JSON object; no result is claimed"
    );
    Ok(value)
}
