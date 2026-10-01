//! Explicitly enabled mainnet signing and emergency-response services.
#![cfg(feature = "mainnet-execution")]

pub mod policy;
pub mod provider;
pub mod watchdog;

use anyhow::{Context, Result, ensure};
use hypercarry_execution::{FileDeadManSwitch, MainnetReleaseEvidence, MainnetReleaseGate};
use hypercarry_executor::{
    config::{self, Config},
    safety::SafetySource,
};
use hypercarry_mainnet_config::{read_json, validate_private_path};
use policy::ServicePolicy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub type Gate = MainnetReleaseGate<SafetySource, FileDeadManSwitch>;

/// # Errors
/// Rejects unavailable or out-of-range system time.
pub fn now_ms() -> Result<u64> {
    u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
        .context("clock overflow")
}

/// # Errors
/// Returns an error if serialization fails.
pub fn digest<T: Serialize>(value: &T) -> Result<String> {
    Ok(hex(&Sha256::digest(serde_json::to_vec(value)?)))
}

/// # Errors
/// Returns an error if the artifact cannot be read.
pub fn artifact_digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let count = file.read(&mut buf)?;
        if count == 0 {
            break;
        }
        hash.update(&buf[..count]);
    }
    Ok(hex(&hash.finalize()))
}

/// The service policy is transitively bound to the existing release digest.
/// This verifies evidence, not operational readiness; cancellation must remain
/// possible when placement health fails.
/// # Errors
/// Rejects invalid configuration, policy digest or release evidence.
pub fn load(
    config_path: &Path,
    policy_path: &Path,
    evidence_path: &Path,
) -> Result<(Config, ServicePolicy, Gate, String)> {
    let config = config::load(config_path)?;
    let policy: ServicePolicy = read_json(policy_path)?;
    policy.validate(&config.runtime)?;
    ensure!(
        config.runtime.services_policy_digest.as_deref() == Some(&digest(&policy)?),
        "service policy is not bound to reviewed runtime"
    );
    let evidence = MainnetReleaseEvidence::load(evidence_path)?;
    let evidence_digest = evidence.evidence_digest()?;
    let gate = MainnetReleaseGate::new(
        config.gate.clone(),
        evidence,
        SafetySource(config.runtime.clone(), config.gate.max_health_age_ms),
        FileDeadManSwitch::new(
            &config.runtime.heartbeat,
            config.runtime.heartbeat_timeout_ms,
        )?,
    )?;
    Ok((config, policy, gate, evidence_digest))
}

/// Owner-only atomic, durable state replacement, including directory fsync.
/// # Errors
/// Returns an error for unsafe paths or failed durable writes.
pub fn write_private<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    validate_private_path(path)?;
    let parent = path.parent().context("state path has no parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Strict existing provider protocol; no key or password fields are accepted.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SigningRequest {
    pub schema_version: u32,
    pub network: hypercarry_execution::ExecutionNetwork,
    pub key_id: String,
    pub signer_address: String,
    pub nonce: u64,
    pub expires_after: u64,
    pub action: serde_json::Value,
}

/// # Errors
/// Rejects unsafe artifacts or a mismatched SHA-256.
pub fn require_artifact(path: &Path, expected: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        path.is_absolute() && fs::symlink_metadata(path)?.file_type().is_file(),
        "artifact must be an absolute regular file"
    );
    ensure!(
        fs::metadata(path)?.permissions().mode() & 0o022 == 0,
        "artifact must not be writable by group/others"
    );
    ensure!(
        artifact_digest(path)? == expected,
        "artifact digest mismatch"
    );
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                char::from(DIGITS[usize::from(b >> 4)]),
                char::from(DIGITS[usize::from(b & 15)]),
            ]
        })
        .collect()
}

/// A bounded, redirect-free venue client. Built once per process rather than per
/// call: each blocking client starts its own runtime thread and loads a root
/// store, and the emergency path cannot afford that work twice per action.
/// # Errors
/// Returns an error if the TLS client cannot be constructed.
pub fn venue_client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(3))
        .build()?)
}

/// Recheck the agent's current account binding without requiring flatness, so
/// emergency actions remain available for filled positions.
/// # Errors
/// Rejects unavailable venue state or an agent mapped to a different account.
pub fn check_agent(
    client: &reqwest::blocking::Client,
    runtime: &hypercarry_mainnet_config::RuntimeConfig,
) -> Result<()> {
    let response = client
        .post("https://api.hyperliquid.xyz/info")
        .json(&serde_json::json!({"type":"userRole","user":runtime.signer_address}))
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response.take(65_537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65_536, "agent response exceeds bound");
    validate_agent_role(&serde_json::from_slice(&bytes)?, &runtime.account_address)
}

fn validate_agent_role(response: &serde_json::Value, account: &str) -> Result<()> {
    ensure!(
        response["role"] == "agent"
            && response
                .pointer("/data/user")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|user| user.eq_ignore_ascii_case(account)),
        "agent is not authorized for the configured account"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn emergency_signing_must_not_follow_a_reassigned_agent() {
        let address = "0x1111111111111111111111111111111111111111";
        assert!(
            validate_agent_role(
                &serde_json::json!({"role":"agent","data":{"user":address}}),
                address
            )
            .is_ok()
        );
        assert!(validate_agent_role(&serde_json::json!({"role":"agent","data":{"user":"0x2222222222222222222222222222222222222222"}}),address).is_err());
        assert!(validate_agent_role(&serde_json::json!({"role":"user"}), address).is_err());
        assert!(validate_agent_role(&serde_json::json!({}), address).is_err());
    }
}
