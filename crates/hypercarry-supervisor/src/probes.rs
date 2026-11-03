//! Observed storage durability and a hash-pinned operational integration.
//! The external probe must check delivery acknowledgements and recovery
//! availability; success is never inferred from process existence or a timer.
use crate::config::ProbeConfig;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

pub fn audit(directory: &Path) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(b"hypercarry supervisor durability probe\n")?;
    file.as_file().sync_all()?;
    fs::File::open(directory)?.sync_all()?;
    file.close()?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    challenge: String,
    integration_config_digest: String,
    alerts_delivered: bool,
    alert_delivery_id: String,
    rollback_ready: bool,
    rollback_artifact_sha256: String,
}

fn validate(bytes: &[u8], challenge: &str, binding: &str) -> Result<()> {
    let receipt: Receipt = serde_json::from_slice(bytes)?;
    ensure!(
        receipt.schema_version == 1
            && receipt.challenge == challenge
            && receipt.integration_config_digest == binding,
        "operational receipt binding mismatch"
    );
    ensure!(
        receipt.alerts_delivered
            && !receipt.alert_delivery_id.trim().is_empty()
            && receipt.rollback_ready,
        "alert delivery or recovery unavailable"
    );
    ensure!(
        receipt.rollback_artifact_sha256.len() == 64
            && receipt
                .rollback_artifact_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit()),
        "missing recovery artifact identity"
    );
    Ok(())
}

pub fn operational(config: &ProbeConfig, binding: &str, stop: &AtomicBool) -> Result<()> {
    hypercarry_supervisor::operations::verify_artifact(&config.executable, &config.sha256)?;
    let mut command = Command::new(&config.executable);
    if let Some(path) = &config.config {
        hypercarry_supervisor::operations::verify_artifact(
            path,
            config
                .config_sha256
                .as_deref()
                .context("missing probe config digest")?,
        )?;
        command.arg("--config").arg(path);
    }
    let nonce = tempfile::NamedTempFile::new()?;
    let challenge = crate::config::hex(&Sha256::digest(format!(
        "{}:{}",
        nonce.path().display(),
        crate::now_ms()?
    )));
    let mut helper = hypercarry_supervisor::process::HelperProcess::spawn(
        command
            .args([
                "--challenge",
                &challenge,
                "--integration-config-digest",
                binding,
            ])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .context("operational probe unavailable")?;
    let child = &mut helper.child;
    let stdout = child.stdout.take().context("missing probe output")?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(8193).read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    let started = Instant::now();
    loop {
        ensure!(
            !stop.load(Ordering::Relaxed)
                && started.elapsed() < Duration::from_millis(config.timeout_ms),
            "operational probe interrupted or timed out"
        );
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "operational probe failed");
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let bytes = receive
        .recv_timeout(Duration::from_millis(100))
        .context("probe output did not close")??;
    ensure!(bytes.len() <= 8192, "probe output exceeds bound");
    validate(&bytes, &challenge, binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_probe_is_hash_pinned_and_receives_only_bound_arguments() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("probe");
        let input = temp.path().join("config");
        let script = r#"#!/bin/sh
[ "$1" = '--config' ] && [ -f "$2" ] && [ "$3" = '--challenge' ] && [ "$5" = '--integration-config-digest' ] || exit 1
printf '{"schema_version":1,"challenge":"%s","integration_config_digest":"%s","alerts_delivered":true,"alert_delivery_id":"fixture","rollback_ready":true,"rollback_artifact_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}' "$4" "$6"
"#;
        fs::write(&executable, script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&input, "{}").unwrap();
        let config = ProbeConfig {
            executable,
            sha256: crate::config::hex(&Sha256::digest(script.as_bytes())),
            timeout_ms: 2000,
            config: Some(input.clone()),
            config_sha256: Some(crate::config::hex(&Sha256::digest(b"{}"))),
        };
        assert!(operational(&config, &"b".repeat(64), &AtomicBool::new(false)).is_ok());
        fs::write(&input, "changed").unwrap();
        assert!(operational(&config, &"b".repeat(64), &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn successful_probe_cannot_leave_a_background_writer() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("probe");
        let marker = temp.path().join("orphan");
        let script = format!(
            r#"#!/bin/sh
(/bin/sleep 0.4; /usr/bin/touch '{}') >/dev/null 2>&1 &
printf '{{"schema_version":1,"challenge":"%s","integration_config_digest":"%s","alerts_delivered":true,"alert_delivery_id":"fixture","rollback_ready":true,"rollback_artifact_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}' "$2" "$4"
"#,
            marker.display()
        );
        fs::write(&executable, &script).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let config = ProbeConfig {
            executable,
            sha256: crate::config::hex(&Sha256::digest(script.as_bytes())),
            timeout_ms: 2000,
            config: None,
            config_sha256: None,
        };
        operational(&config, &"b".repeat(64), &AtomicBool::new(false)).unwrap();
        thread::sleep(Duration::from_millis(550));
        assert!(
            !marker.exists(),
            "probe descendant survived a successful response"
        );
    }

    #[test]
    fn receipts_must_prove_delivery_and_bind_the_current_challenge() {
        let mut value = serde_json::json!({"schema_version":1,"challenge":"fresh","integration_config_digest":"binding","alerts_delivered":true,"alert_delivery_id":"delivery/1","rollback_ready":true,"rollback_artifact_sha256":"a".repeat(64)});
        assert!(validate(&serde_json::to_vec(&value).unwrap(), "fresh", "binding").is_ok());
        assert!(validate(&serde_json::to_vec(&value).unwrap(), "old", "binding").is_err());
        assert!(validate(&serde_json::to_vec(&value).unwrap(), "fresh", "other").is_err());
        value["alerts_delivered"] = false.into();
        assert!(validate(&serde_json::to_vec(&value).unwrap(), "fresh", "binding").is_err());
    }
}
