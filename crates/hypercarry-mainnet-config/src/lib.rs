//! Mainnet runtime configuration and the digest that binds it to a release.
//!
//! This crate exists because more than one process must agree, byte for byte,
//! on what the reviewed runtime configuration *is*. The executor refuses a
//! health file whose `integration_config_digest` differs from its own, so the
//! supervisor that writes that file has to compute the identical digest. The
//! digest is a hash of this type's JSON serialization, which makes it sensitive
//! to field order and serde attributes — two independent declarations of the
//! same shape would agree until the day one of them gained a field.
//!
//! Holding the definition once removes that failure mode by construction.
//!
//! Nothing here can trade. There is no signer, no transport, and no venue
//! dependency: this crate reads and hashes a configuration, and that is all.
#![warn(missing_docs)]

#[cfg(feature = "mainnet-execution")]
pub mod health;

use anyhow::{Context, Result, ensure};
use hypercarry_execution::{MarketMetadata, RiskLimits, ValidatedOrder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

/// Reads the `runtime` section of an executor configuration file.
///
/// The surrounding envelope — schema version and release gate — belongs to the
/// process that enforces it, and its type is gated behind the mainnet feature.
/// Taking only the runtime section keeps this crate free of that gate, so a
/// default workspace build still compiles it and the default-off property of
/// the workspace is unchanged.
///
/// The section itself is deserialized strictly: unknown fields are rejected,
/// because a field this crate ignores is a field the digest does not cover.
///
/// # Errors
///
/// Returns an error for an unreadable, oversized or malformed file, or one
/// with no `runtime` section.
pub fn read_runtime(path: &Path) -> Result<RuntimeConfig> {
    let document: serde_json::Value = read_json(path)?;
    let runtime = document
        .get("runtime")
        .context("configuration has no runtime section")?;
    serde_json::from_value(runtime.clone()).context("invalid runtime configuration")
}

/// Every operational parameter covered by the reviewed configuration digest.
///
/// Field order is part of the contract. Reordering, adding, or removing a
/// field changes the digest of every existing configuration and invalidates
/// every release bundle reviewed against it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    /// Public trading account identity.
    pub account_address: String,
    /// Public address of the pinned external agent signer.
    pub signer_address: String,
    /// Non-secret alias naming the signer provider.
    pub signer_alias: String,
    /// Owner-only Unix socket the external signer listens on.
    pub signer_socket: PathBuf,
    /// Bounded signer response timeout, milliseconds.
    pub signer_timeout_ms: u64,
    /// Bounded signed-action expiry, milliseconds.
    pub action_ttl_ms: u64,
    /// Durable execution journal path.
    pub journal: PathBuf,
    /// Health file written by the independent supervisor.
    pub health_file: PathBuf,
    /// Dead-man heartbeat observed by the executor.
    pub heartbeat: PathBuf,
    /// Maximum tolerated heartbeat age, milliseconds.
    pub heartbeat_timeout_ms: u64,
    /// Kill-switch path; its existence disables execution.
    pub kill_switch: PathBuf,
    /// Reviewed venue asset identifier.
    pub asset_id: u32,
    /// Venue size precision for the reviewed market.
    pub size_decimals: u32,
    /// Reviewed market metadata used to quantize the order.
    pub metadata: MarketMetadata,
    /// The exact order this configuration authorizes.
    pub order: ValidatedOrder,
    /// Deterministic risk limits.
    pub risk: RiskLimits,
    /// Bounded reconciliation attempts.
    pub reconciliation_attempts: u32,
    /// Interval between reconciliation attempts, milliseconds.
    pub reconciliation_interval_ms: u64,
    /// Optional SHA-256 of the serialized external service policy. Absent on
    /// historical configurations; adding it requires a new release review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub services_policy_digest: Option<String>,
    /// Liveness lease renewed only by the running executor, consumed by a
    /// watchdog on an independent host. Included in new reviewed digests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_lease: Option<PathBuf>,
}

impl RuntimeConfig {
    /// Digest binding this exact runtime configuration to a reviewed release.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be serialized.
    pub fn digest(&self) -> Result<String> {
        Ok(hex(&Sha256::digest(serde_json::to_vec(self)?)))
    }
}

/// Reads a strict JSON document from a regular file, bounded to 1 MiB.
///
/// # Errors
///
/// Returns an error for a symlink or special file, an oversized file, or
/// content that is not exactly the expected shape.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_file(),
        "JSON inputs must be regular files, not symlinks or special files"
    );
    let mut data = Vec::new();
    File::open(path)?.take(1_048_577).read_to_end(&mut data)?;
    ensure!(data.len() <= 1_048_576, "JSON input exceeds 1 MiB");
    serde_json::from_slice(&data).context("invalid strict JSON input")
}

/// Lowercase hexadecimal, without pulling in a dependency for it.
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

/// Requires a runtime path to live in a private directory and not be a symlink.
///
/// # Errors
///
/// Returns an error for a missing or world-readable parent, a symlinked parent
/// or target, or a non-Unix host.
#[cfg(unix)]
pub fn validate_private_path(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path.parent().context("runtime path has no parent")?;
    let meta = std::fs::symlink_metadata(parent).context("runtime parent is unavailable")?;
    ensure!(
        meta.is_dir()
            && !meta.file_type().is_symlink()
            && meta.permissions().mode().trailing_zeros() >= 6,
        "runtime parent must be a private directory (0700)"
    );
    match std::fs::symlink_metadata(path) {
        Ok(meta) => ensure!(
            !meta.file_type().is_symlink(),
            "runtime paths must not be symlinks"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Mainnet operation requires Unix filesystem semantics.
///
/// # Errors
///
/// Always returns an error on non-Unix hosts.
#[cfg(not(unix))]
pub fn validate_private_path(_: &Path) -> Result<()> {
    anyhow::bail!("mainnet executor requires Unix")
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

    /// The digest of a fixed configuration is a permanent commitment.
    ///
    /// Every reviewed release bundle records the digest of the configuration it
    /// approved, and the executor refuses a health file that does not carry the
    /// same value. A change to this type's field order or serde attributes
    /// would silently invalidate all of them, and would do so at the moment an
    /// operator most needs the binding to hold. Pinning the value here turns
    /// that into a failing test instead.
    ///
    /// This value was captured before the type moved out of the executor crate,
    /// and proves the move did not disturb it.
    #[test]
    fn the_reviewed_digest_of_a_fixed_configuration_never_changes() {
        assert_eq!(
            runtime().digest().unwrap(),
            "29691197cf67f554c3f662787b4e5a6b57b94f3a5cbd6c53b3b6d0203b84fc3b"
        );
    }

    #[test]
    fn every_operational_change_changes_reviewed_digest() {
        let original = runtime();
        let baseline = original.digest().unwrap();
        let mut variants = Vec::new();
        let mut value = original.clone();
        value.signer_address = "0x3333333333333333333333333333333333333333".into();
        variants.push(value);
        let mut value = original.clone();
        value.order.limit_price += rust_decimal::Decimal::ONE;
        variants.push(value);
        let mut value = original.clone();
        value.health_file = "/another/health.json".into();
        variants.push(value);
        let mut value = original.clone();
        value.risk.max_leverage += rust_decimal::Decimal::ONE;
        variants.push(value);
        let mut value = original.clone();
        value.asset_id = 1;
        variants.push(value);
        for variant in variants {
            assert_ne!(baseline, variant.digest().unwrap());
        }
    }

    #[test]
    fn runtime_rejects_unknown_secret_or_endpoint_fields() {
        let baseline = serde_json::to_value(runtime()).unwrap();
        for field in ["private_key", "endpoint", "enable_mainnet"] {
            let mut value = baseline.clone();
            value[field] = "unsupported".into();
            assert!(serde_json::from_value::<RuntimeConfig>(value).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_boundary_rejects_symlinks_and_shared_parent() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().join("journal");
        assert!(validate_private_path(&path).is_ok());
        symlink(temp.path().join("target"), &path).unwrap();
        assert!(validate_private_path(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_private_path(&path).is_err());
    }
}
