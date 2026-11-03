//! Read-only operations probe: checks real monitoring receipts and recovery bytes.
//! Receipt writers are trusted infrastructure; this module never fabricates them.
use anyhow::{Context, Result, ensure};
use hypercarry_mainnet_config::{read_json, validate_disjoint_paths, validate_private_path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::PathBuf};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub alert_receipt: PathBuf,
    pub recovery_receipt: PathBuf,
    pub max_alert_age_ms: u64,
    pub max_recovery_age_ms: u64,
    pub recovery_artifact: PathBuf,
    pub recovery_artifact_sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AlertReceipt {
    schema_version: u32,
    integration_config_digest: String,
    delivered_at_ms: i64,
    delivered: bool,
    delivery_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryReceipt {
    schema_version: u32,
    integration_config_digest: String,
    checked_at_ms: i64,
    access_verified: bool,
    check_id: String,
    artifact_sha256: String,
}

#[derive(Serialize)]
pub struct Observation {
    schema_version: u32,
    challenge: String,
    integration_config_digest: String,
    alerts_delivered: bool,
    alert_delivery_id: String,
    rollback_ready: bool,
    rollback_artifact_sha256: String,
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl Config {
    /// Validate offline configuration without creating state or claiming readiness.
    /// # Errors
    /// Rejects unsafe paths, invalid ages and malformed digests.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "unsupported operations probe schema"
        );
        ensure!(
            (1_000..=3_600_000).contains(&self.max_alert_age_ms)
                && (1_000..=3_600_000).contains(&self.max_recovery_age_ms),
            "receipt age limits must be 1 second to 1 hour"
        );
        ensure!(
            sha256(&self.recovery_artifact_sha256),
            "invalid recovery digest"
        );
        validate_disjoint_paths([
            self.alert_receipt.as_path(),
            self.recovery_receipt.as_path(),
            self.recovery_artifact.as_path(),
        ])?;
        for path in [&self.alert_receipt, &self.recovery_receipt] {
            validate_private_path(path)?;
        }
        Ok(())
    }
}

fn fresh(at: i64, now: i64, max_age: u64) -> Result<()> {
    ensure!(at > 0 && at <= now, "missing or future receipt timestamp");
    ensure!(
        u64::try_from(now - at)? <= max_age,
        "operational receipt is stale"
    );
    Ok(())
}

/// Hash a regular, non-group/world-writable artifact using bounded memory.
/// # Errors
/// Rejects unsafe artifacts, unreadable bytes and incorrect digests.
pub fn verify_artifact(path: &std::path::Path, expected: &str) -> Result<()> {
    ensure!(
        path.is_absolute() && sha256(expected),
        "invalid artifact identity"
    );
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.file_type().is_file(),
        "artifact must be a regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o022 == 0,
            "artifact is group/world writable"
        );
    }
    let mut hash = Sha256::new();
    let mut file = fs::File::open(path)?;
    let mut buffer = [0; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    ensure!(
        crate::config::hex(&hash.finalize()) == expected,
        "artifact digest mismatch"
    );
    Ok(())
}

/// Validate current delivery/recovery receipts and produce the supervisor response.
/// # Errors
/// Rejects stale, unbound or unsuccessful receipts and missing/changed artifacts.
pub fn observe(
    config: &Config,
    challenge: &str,
    binding: &str,
    clock: impl FnOnce() -> Result<i64>,
) -> Result<Observation> {
    config.validate()?;
    ensure!(
        sha256(challenge) && sha256(binding),
        "invalid challenge or runtime digest"
    );
    let alert: AlertReceipt =
        read_json(&config.alert_receipt).context("alert receipt unavailable")?;
    let recovery: RecoveryReceipt =
        read_json(&config.recovery_receipt).context("recovery receipt unavailable")?;
    ensure!(
        alert.schema_version == 1 && recovery.schema_version == 1,
        "unsupported receipt schema"
    );
    ensure!(
        alert.integration_config_digest == binding && recovery.integration_config_digest == binding,
        "receipt runtime mismatch"
    );
    ensure!(
        alert.delivered && !alert.delivery_id.trim().is_empty(),
        "no confirmed alert delivery"
    );
    ensure!(
        recovery.access_verified && !recovery.check_id.trim().is_empty(),
        "recovery access not verified"
    );
    ensure!(
        recovery.artifact_sha256 == config.recovery_artifact_sha256,
        "recovery receipt names another artifact"
    );
    verify_artifact(&config.recovery_artifact, &config.recovery_artifact_sha256)?;
    // Hashing a recovery archive can take longer than the receipt's lifetime.
    // Evaluate age after all I/O rather than against the process start time.
    let now = clock()?;
    fresh(alert.delivered_at_ms, now, config.max_alert_age_ms)?;
    fresh(recovery.checked_at_ms, now, config.max_recovery_age_ms)?;
    Ok(Observation {
        schema_version: 1,
        challenge: challenge.into(),
        integration_config_digest: binding.into(),
        alerts_delivered: true,
        alert_delivery_id: alert.delivery_id,
        rollback_ready: true,
        rollback_artifact_sha256: config.recovery_artifact_sha256.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_fresh_bound_receipts_and_exact_recovery_bytes_report_ready() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let config = Config {
            schema_version: 1,
            alert_receipt: temp.path().join("alert"),
            recovery_receipt: temp.path().join("recovery"),
            max_alert_age_ms: 1000,
            max_recovery_age_ms: 2000,
            recovery_artifact: temp.path().join("artifact"),
            recovery_artifact_sha256: crate::config::hex(&Sha256::digest(b"reviewed bytes")),
        };
        fs::write(&config.recovery_artifact, b"reviewed bytes").unwrap();
        let binding = "a".repeat(64);
        let challenge = "b".repeat(64);
        let mut alert = AlertReceipt {
            schema_version: 1,
            integration_config_digest: binding.clone(),
            delivered_at_ms: 10_000,
            delivered: true,
            delivery_id: "confirmed/1".into(),
        };
        let mut recovery = RecoveryReceipt {
            schema_version: 1,
            integration_config_digest: binding.clone(),
            checked_at_ms: 10_000,
            access_verified: true,
            check_id: "checked/1".into(),
            artifact_sha256: config.recovery_artifact_sha256.clone(),
        };
        let write = |a: &AlertReceipt, r: &RecoveryReceipt| {
            fs::write(&config.alert_receipt, serde_json::to_vec(a).unwrap()).unwrap();
            fs::write(&config.recovery_receipt, serde_json::to_vec(r).unwrap()).unwrap();
        };
        assert!(observe(&config, &challenge, &binding, || Ok(10_500)).is_err());
        write(&alert, &recovery);
        let response =
            serde_json::to_value(observe(&config, &challenge, &binding, || Ok(10_500)).unwrap())
                .unwrap();
        assert_eq!(response["challenge"], challenge);
        assert!(observe(&config, &challenge, &"c".repeat(64), || Ok(10_500)).is_err());
        assert!(observe(&config, &challenge, &binding, || Ok(9_999)).is_err());
        assert!(observe(&config, &challenge, &binding, || Ok(11_001)).is_err());
        // The clock must be read after artifact verification, once, and clock
        // failure must never turn expired receipts into an affirmative result.
        assert!(
            observe(&config, &challenge, &binding, || anyhow::bail!(
                "clock unavailable"
            ))
            .is_err()
        );
        alert.delivered = false;
        write(&alert, &recovery);
        assert!(observe(&config, &challenge, &binding, || Ok(10_500)).is_err());
        alert.delivered = true;
        recovery.checked_at_ms = 8_499;
        write(&alert, &recovery);
        assert!(observe(&config, &challenge, &binding, || Ok(10_500)).is_err());
        recovery.checked_at_ms = 10_000;
        recovery.access_verified = false;
        write(&alert, &recovery);
        assert!(observe(&config, &challenge, &binding, || Ok(10_500)).is_err());
        recovery.access_verified = true;
        recovery.artifact_sha256 = "c".repeat(64);
        write(&alert, &recovery);
        assert!(observe(&config, &challenge, &binding, || Ok(10_500)).is_err());
        recovery
            .artifact_sha256
            .clone_from(&config.recovery_artifact_sha256);
        write(&alert, &recovery);
        fs::write(&config.recovery_artifact, "changed bytes").unwrap();
        assert!(
            observe(&config, &challenge, &binding, || panic!(
                "clock read before artifact verification"
            ))
            .is_err()
        );
    }
}
