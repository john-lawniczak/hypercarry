//! Single-use, short-lived operator consent for one exact reviewed MCP action.
//! This is additional consent, never a replacement for release evidence or health.
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use hypercarry_mainnet_config::{RuntimeConfig, read_json, validate_private_path};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::Path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOperation {
    Run,
    Recover,
}
impl ApprovalOperation {
    pub const fn phrase(self) -> &'static str {
        match self {
            Self::Run => hypercarry_execution::MAINNET_CONFIRMATION,
            Self::Recover => "RECOVER HYPERCARRY MAINNET ORDERS",
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    schema_version: u32,
    network: String,
    operation: ApprovalOperation,
    integration_config_digest: String,
    evidence_digest: String,
    issued_at_ms: i64,
    expires_at_ms: i64,
    confirmation: String,
}

/// Only consumption of a validated, atomically claimed file creates this proof.
pub struct ConsumedApproval {
    confirmation: String,
}
impl ConsumedApproval {
    pub fn confirmation(&self) -> &str {
        &self.confirmation
    }
}

/// Issue consent after the caller obtains the exact operator confirmation.
/// # Errors
/// Refuses an incorrect phrase, unsafe output, overwrite or TTL outside 1..300s.
pub fn issue(
    path: &Path,
    runtime: &RuntimeConfig,
    evidence_digest: &str,
    operation: ApprovalOperation,
    confirmation: String,
    now: i64,
    ttl_ms: i64,
) -> Result<()> {
    ensure!(
        confirmation == operation.phrase(),
        "incorrect mainnet confirmation"
    );
    ensure!(
        now >= 0 && (1_000..=300_000).contains(&ttl_ms),
        "approval TTL must be 1..300s"
    );
    validate_private_path(path)?;
    ensure!(path.is_absolute(), "approval path must be absolute");
    let consumed = path.with_extension("consumed");
    ensure!(
        !consumed.exists(),
        "approval ID was already consumed; use a new ID"
    );
    let approval = Approval {
        schema_version: 1,
        network: "mainnet".into(),
        operation,
        integration_config_digest: runtime.digest()?,
        evidence_digest: evidence_digest.into(),
        issued_at_ms: now,
        expires_at_ms: now
            .checked_add(ttl_ms)
            .context("approval expiry overflow")?,
        confirmation,
    };
    let parent = path.parent().context("approval has no parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, &approval)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn validate(
    approval: &Approval,
    runtime: &RuntimeConfig,
    evidence_digest: &str,
    operation: ApprovalOperation,
    now: i64,
) -> Result<()> {
    ensure!(
        approval.schema_version == 1
            && approval.network == "mainnet"
            && approval.operation == operation,
        "approval operation/network mismatch"
    );
    ensure!(
        approval.confirmation == operation.phrase(),
        "approval has no explicit confirmation"
    );
    ensure!(
        approval.integration_config_digest == runtime.digest()?
            && approval.evidence_digest == evidence_digest,
        "approval belongs to another runtime/release"
    );
    ensure!(
        approval.issued_at_ms >= 0
            && approval.issued_at_ms <= now
            && approval.expires_at_ms > now
            && (1_000..=300_000).contains(
                &approval
                    .expires_at_ms
                    .checked_sub(approval.issued_at_ms)
                    .context("invalid approval interval")?
            ),
        "approval expired or future-dated"
    );
    Ok(())
}

/// Atomically claims an approval before any external effect. The retained
/// `.consumed` file permanently prevents replay even after an ambiguous failure.
/// # Errors
/// Rejects unsafe, expired, mismatched or previously consumed approvals.
pub fn consume(
    path: &Path,
    runtime: &RuntimeConfig,
    evidence_digest: &str,
    operation: ApprovalOperation,
    now: i64,
) -> Result<ConsumedApproval> {
    validate_private_path(path)?;
    ensure!(path.is_absolute(), "approval path must be absolute");
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file() && metadata.permissions().mode().trailing_zeros() >= 6,
        "approval must be an owner-only regular file"
    );
    let approval: Approval = read_json(path)?;
    validate(&approval, runtime, evidence_digest, operation, now)?;
    let consumed = path.with_extension("consumed");
    // link(2) fails if destination exists; unlike rename it cannot overwrite a
    // prior claim. Both names share an inode until source removal is durable.
    fs::hard_link(path, &consumed).context("approval is already claimed or cannot be retained")?;
    File::open(path.parent().context("approval has no parent")?)?.sync_all()?;
    let claimed: Approval = read_json(&consumed)?;
    validate(&claimed, runtime, evidence_digest, operation, now)?;
    fs::remove_file(path)?;
    File::open(path.parent().context("approval has no parent")?)?.sync_all()?;
    Ok(ConsumedApproval {
        confirmation: claimed.confirmation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn runtime() -> RuntimeConfig {
        serde_json::from_str(include_str!(
            "../../../docs/mainnet-runtime-config.example.json"
        ))
        .unwrap()
    }
    #[test]
    fn approval_is_bound_expiring_and_single_use() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().join("one.json");
        let runtime = runtime();
        issue(
            &path,
            &runtime,
            "reviewed",
            ApprovalOperation::Run,
            ApprovalOperation::Run.phrase().into(),
            1000,
            1000,
        )
        .unwrap();
        assert!(consume(&path, &runtime, "other", ApprovalOperation::Run, 1000).is_err());
        assert!(
            consume(
                &path,
                &runtime,
                "reviewed",
                ApprovalOperation::Recover,
                1000
            )
            .is_err()
        );
        assert!(consume(&path, &runtime, "reviewed", ApprovalOperation::Run, 999).is_err());
        assert!(consume(&path, &runtime, "reviewed", ApprovalOperation::Run, 2000).is_err());
        let bytes = fs::read(&path).unwrap();
        consume(&path, &runtime, "reviewed", ApprovalOperation::Run, 1500).unwrap();
        assert!(!path.exists());
        assert!(path.with_extension("consumed").exists());
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(consume(&path, &runtime, "reviewed", ApprovalOperation::Run, 1500).is_err());
    }
    #[test]
    fn concurrent_consumers_cannot_both_claim_one_action() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().join("one.json");
        issue(
            &path,
            &runtime(),
            "reviewed",
            ApprovalOperation::Run,
            ApprovalOperation::Run.phrase().into(),
            1000,
            1000,
        )
        .unwrap();
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    consume(&path, &runtime(), "reviewed", ApprovalOperation::Run, 1500).is_ok()
                })
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .filter_map(|t| t.join().ok())
                .filter(|ok| *ok)
                .count(),
            1
        );
    }
}
