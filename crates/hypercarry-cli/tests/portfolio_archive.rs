//! Exercise the real CLI failure boundary without a venue connection.
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

fn failed_read(root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hypercarry"))
        .env_clear()
        // Refused local proxy: no traffic reaches a venue and no credentials exist.
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .args([
            "account",
            "--network",
            "testnet",
            "--address",
            "0x1111111111111111111111111111111111111111",
            "--output",
            "json",
            "portfolio",
            "--require-exits",
            "--archive-dir",
        ])
        .arg(root)
        .output()
        .unwrap()
}

#[test]
fn failed_read_persists_unknown_coverage_and_never_prints_success() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let result = failed_read(dir.path());
    assert_eq!(
        result.status.code(),
        Some(14),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, [] as [u8; 0]);
    let files: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect();
    // The immutable observation, the alert-tracking lock/state pair, the
    // appended notification journal, and the collector heartbeat file.
    assert_eq!(files.len(), 5, "{files:?}");
    let observation = files
        .iter()
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("Z-observation-")
        })
        .expect("observation file must exist");
    assert_eq!(
        fs::metadata(observation).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(observation).unwrap()).unwrap();
    assert!(saved["snapshot"].is_null());
    assert!(saved["exit_coverage_observed"].is_null());
    assert_ne!(saved["read_error"].as_str().unwrap(), "");

    let journal = fs::read_to_string(dir.path().join("notifications.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 1);
    let line: serde_json::Value = serde_json::from_str(journal.lines().next().unwrap()).unwrap();
    assert_eq!(line["notification"]["kind"], "state_unknown");
    assert_eq!(line["notification"]["coin"], "__account__");
    assert!(dir.path().join(".portfolio-heartbeat.json").is_file());
    assert!(dir.path().join(".portfolio-alerts-state.json").is_file());
}

#[test]
fn storage_failure_preserves_read_failure_context_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let result = failed_read(dir.path());
    assert_eq!(result.status.code(), Some(13));
    assert_eq!(result.stdout, [] as [u8; 0]);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    let error = String::from_utf8(result.stderr).unwrap();
    assert!(error.contains("archive failed:"));
    assert!(error.contains("read error:"));
    assert!(!error.contains("read error: none"));
}
