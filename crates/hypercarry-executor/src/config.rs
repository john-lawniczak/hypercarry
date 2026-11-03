//! Executor-side configuration policy.
//!
//! The configuration types and the reviewed digest live in
//! `hypercarry-mainnet-config`, because the independent supervisor must compute
//! the identical digest for the health file this process will accept. What
//! stays here is what only this process can decide: whether a configuration is
//! valid to execute, and whether this binary is the reviewed one.

use anyhow::{Result, ensure};
use hypercarry_execution::{
    MainnetReleaseConfig, MainnetReleaseEvidence, OrderIntent, ValidatedOrder,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub use hypercarry_mainnet_config::{RuntimeConfig, read_json, validate_private_path};

/// Complete executor configuration: the release gate and the runtime it binds.
///
/// The envelope stays here because the release gate is this process's concern;
/// the runtime section it carries is defined once, in the shared crate, so the
/// supervisor computes the identical digest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Version of the executor configuration contract.
    pub schema_version: u32,
    /// Reviewed release-gate parameters.
    pub gate: MainnetReleaseConfig,
    /// Complete runtime configuration covered by the reviewed digest.
    pub runtime: RuntimeConfig,
}

/// Reads and fully validates an executor configuration.
/// # Errors
/// Rejects unreadable configuration or an invalid runtime/release binding.
pub fn load(path: &Path) -> Result<Config> {
    let config: Config = read_json(path)?;
    validate(&config)?;
    validate_runtime_inputs(&config.runtime, &[path])?;
    Ok(config)
}

/// Keep release evidence and approval files separate from runtime writers.
/// # Errors
/// Rejects aliases between inputs, mutable state and the supervisor lock.
pub fn validate_runtime_inputs(r: &RuntimeConfig, inputs: &[&Path]) -> Result<()> {
    let supervisor_lock = r.health_file.with_extension("supervisor.lock");
    hypercarry_mainnet_config::validate_disjoint_paths(
        [
            &r.signer_socket,
            &r.journal,
            &r.health_file,
            &r.heartbeat,
            &r.kill_switch,
            &supervisor_lock,
        ]
        .into_iter()
        .map(std::path::PathBuf::as_path)
        .chain(r.executor_lease.as_deref())
        .chain(inputs.iter().copied()),
    )
}

/// Rejects any configuration this process must not execute.
/// # Errors
/// Rejects invalid limits, paths, identities or configuration digests.
pub fn validate(config: &Config) -> Result<()> {
    let r = &config.runtime;
    ensure!(config.schema_version == 1, "unsupported executor schema");
    config.gate.validate()?;
    ensure!(
        r.services_policy_digest.is_some() == r.executor_lease.is_some(),
        "service policy and executor lease must be configured together"
    );
    if let Some(path) = &r.executor_lease {
        ensure!(path.is_absolute(), "executor lease must be absolute");
        validate_private_path(path)?;
        ensure!(
            ![
                &r.signer_socket,
                &r.journal,
                &r.health_file,
                &r.heartbeat,
                &r.kill_switch
            ]
            .contains(&path),
            "executor lease must have a distinct path"
        );
    }
    r.risk.validate()?;
    r.order.validate()?;
    let intent = OrderIntent::limit(
        r.order.correlation_id.clone(),
        r.order.venue.clone(),
        r.order.market.clone(),
        r.order.side,
        r.order.quantity,
        r.order.limit_price,
        r.order.created_at_ms,
    )?;
    ensure!(
        ValidatedOrder::resolve(&intent, &r.metadata)? == r.order,
        "configured order is not exactly quantized to reviewed metadata"
    );
    let digest = r.digest()?;
    ensure!(
        config.gate.integration_config_digest.as_deref() == Some(&digest),
        "complete runtime config is not bound to release gate"
    );
    hypercarry_hyperliquid::HyperliquidMainnetConfig::new(
        r.account_address.clone(),
        r.signer_address.clone(),
        r.action_ttl_ms,
    )?;
    ensure!(
        r.order.venue == "hyperliquid"
            && config.gate.allowed_market == format!("hyperliquid:{}", r.order.market),
        "order market differs from release gate"
    );
    ensure!(
        r.metadata.venue == r.order.venue && r.metadata.market == r.order.market,
        "metadata identity mismatch"
    );
    ensure!(r.size_decimals <= 6, "unsupported perp size precision");
    let step = rust_decimal::Decimal::new(1, r.size_decimals);
    ensure!(
        r.order.quantity % step == rust_decimal::Decimal::ZERO,
        "order size violates venue precision"
    );
    ensure!(
        r.signer_timeout_ms > 0 && r.signer_timeout_ms <= 60_000,
        "invalid signer timeout"
    );
    ensure!(
        r.heartbeat_timeout_ms > 0 && r.heartbeat_timeout_ms <= 60_000,
        "invalid heartbeat timeout"
    );
    ensure!(
        r.reconciliation_attempts > 0 && r.reconciliation_attempts <= 60,
        "invalid reconciliation attempts"
    );
    ensure!(
        (1_000..=10_000).contains(&r.reconciliation_interval_ms),
        "invalid reconciliation interval"
    );
    validate_runtime_paths(r)
}

fn validate_runtime_paths(r: &RuntimeConfig) -> Result<()> {
    let supervisor_lock = r.health_file.with_extension("supervisor.lock");
    let paths = [
        &r.signer_socket,
        &r.journal,
        &r.health_file,
        &r.heartbeat,
        &r.kill_switch,
        &supervisor_lock,
    ];
    hypercarry_mainnet_config::validate_disjoint_paths(
        paths
            .iter()
            .map(|p| p.as_path())
            .chain(r.executor_lease.as_deref()),
    )?;
    for (index, path) in paths.iter().enumerate() {
        ensure!(path.is_absolute(), "runtime paths must be absolute");
        validate_private_path(path)?;
        ensure!(
            !paths[..index].contains(path),
            "runtime paths must be distinct"
        );
        ensure!(
            !path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "runtime paths must not contain parent traversal"
        );
    }
    Ok(())
}

/// Requires this running binary to be the reviewed one, built from clean source.
/// # Errors
/// Rejects dirty, unreviewed or mismatched source and executable artifacts.
pub fn verify_build(config: &Config, evidence: &MainnetReleaseEvidence) -> Result<()> {
    ensure!(
        env!("HYPERCARRY_BUILD_CLEAN") == "true",
        "mainnet requires a binary built from a clean committed tree"
    );
    ensure!(
        config.gate.source_commit == env!("HYPERCARRY_BUILD_COMMIT"),
        "source commit differs from compiled identity"
    );
    ensure!(
        config.gate.cargo_lock_digest == env!("HYPERCARRY_BUILD_LOCK"),
        "lockfile differs from compiled identity"
    );
    let binary = File::open(std::env::current_exe()?)?;
    let mut reader = std::io::BufReader::new(binary);
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    ensure!(
        evidence.reviewed_bundle.mainnet_transport_artifact
            == format!("sha256/{}", hex(&hash.finalize())),
        "executor binary differs from reviewed transport artifact"
    );
    ensure!(
        evidence.reviewed_bundle.mainnet_signer_key_id == config.runtime.signer_alias,
        "signer alias differs from reviewed bundle"
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

#[cfg(test)]
mod input_tests {
    use super::*;

    #[test]
    fn evidence_and_approval_paths_cannot_alias_lease_or_lock_even_before_creation() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let mut runtime: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../docs/mainnet-runtime-config.example.json"
        ))
        .unwrap();
        runtime.signer_socket = root.join("socket");
        runtime.journal = root.join("journal");
        runtime.health_file = root.join("health");
        runtime.heartbeat = root.join("heartbeat");
        runtime.kill_switch = root.join("STOP");
        runtime.executor_lease = Some(root.join("lease"));
        let evidence = root.join("evidence");
        assert!(validate_runtime_inputs(&runtime, &[evidence.as_path()]).is_ok());
        for collision in [
            runtime.journal.clone(),
            runtime.executor_lease.clone().unwrap(),
            runtime.health_file.with_extension("supervisor.lock"),
        ] {
            assert!(
                validate_runtime_inputs(&runtime, &[evidence.as_path(), collision.as_path()])
                    .is_err()
            );
        }
        std::fs::write(&evidence, "retained evidence").unwrap();
        let alias = root.join("approval");
        std::fs::hard_link(&evidence, &alias).unwrap();
        assert!(validate_runtime_inputs(&runtime, &[evidence.as_path(), alias.as_path()]).is_err());
        assert_eq!(
            std::fs::read_to_string(evidence).unwrap(),
            "retained evidence"
        );
        assert!(!runtime.executor_lease.unwrap().exists());
    }
}
