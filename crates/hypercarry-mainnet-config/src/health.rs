//! The health file: a wire contract between two processes.
//!
//! The independent supervisor writes it; the executor refuses to act without
//! it. Defining it once, here, means the writer cannot drift from the reader —
//! a field added on one side would otherwise compile cleanly and fail at
//! runtime, by rejecting every health file, in the situation the file exists to
//! make safe.
//!
//! The executor applies its own checks on top of what this module enforces:
//! it re-reads the file at the authorization boundary, measures its own live
//! account-check latency, and re-tests freshness after that I/O. Passing the
//! checks here is necessary, not sufficient.

use crate::RuntimeConfig;
use anyhow::{Context, Result, ensure};
use hypercarry_execution::{ExecutionNetwork, OperationalHealth, RiskSnapshot};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};

/// Version of the health-file contract.
pub const HEALTH_SCHEMA_VERSION: u32 = 1;

/// One observation of operational and risk state, bound to an exact account
/// and an exact reviewed configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthEnvelope {
    /// Version of this contract.
    pub schema_version: u32,
    /// Deployment the observation was made against.
    pub network: ExecutionNetwork,
    /// Public trading account the observation describes.
    pub account_address: String,
    /// Digest of the runtime configuration this observation is bound to.
    pub integration_config_digest: String,
    /// Whether the account holds no position on any DEX.
    ///
    /// This is the supervisor's claim, and it is the one the executor cannot
    /// make for itself: its own REST check covers the default perp DEX, which
    /// does not establish flatness for a unified account.
    pub account_flat: bool,
    /// Operational readiness at `health.observed_at_ms`.
    pub health: OperationalHealth,
    /// Account and market state at the same instant.
    pub risk: RiskSnapshot,
}

impl HealthEnvelope {
    /// Rejects an envelope that does not describe the expected account and
    /// configuration, or that was not observed at a single instant.
    ///
    /// The timestamps must be equal rather than merely close. Health and risk
    /// are read together by the gate and compared against one freshness bound,
    /// so two instants would let a stale half hide behind a fresh one.
    ///
    /// # Errors
    ///
    /// Returns an error for a schema, network, account, digest or timestamp
    /// mismatch.
    pub fn validate_binding(&self, config: &RuntimeConfig) -> Result<()> {
        ensure!(
            self.schema_version == HEALTH_SCHEMA_VERSION
                && self.network == ExecutionNetwork::Mainnet,
            "health identity mismatch"
        );
        ensure!(
            self.account_address == config.account_address
                && self.integration_config_digest == config.digest()?,
            "health account/config mismatch"
        );
        ensure!(
            self.risk.observed_at_ms == self.health.observed_at_ms,
            "risk and health timestamps differ"
        );
        Ok(())
    }

    /// Replaces the health file atomically, owner-only.
    ///
    /// The executor may read this path at any moment, so a reader must see
    /// either the whole previous observation or the whole new one. The
    /// temporary file is created in the destination directory — a rename
    /// across filesystems is not atomic — and is flushed before the rename, so
    /// a crash cannot leave a renamed file with unwritten contents.
    ///
    /// # Errors
    ///
    /// Returns an error for a path with no parent directory, or any I/O
    /// failure while writing, flushing or renaming.
    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        ensure!(path.parent().is_some(), "health path has no parent");
        // Same directory, therefore the same filesystem: `rename` is only
        // atomic within one.
        let temporary = path.with_extension("tmp");
        let mut file = fs::File::create(&temporary)
            .with_context(|| format!("cannot create {}", temporary.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(&serde_json::to_vec(self)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
            .with_context(|| format!("cannot replace {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercarry_execution::Readiness;
    use rust_decimal::Decimal;

    fn runtime() -> RuntimeConfig {
        serde_json::from_str(include_str!(
            "../../../docs/mainnet-runtime-config.example.json"
        ))
        .unwrap()
    }

    fn envelope(config: &RuntimeConfig, at_ms: i64) -> HealthEnvelope {
        HealthEnvelope {
            schema_version: HEALTH_SCHEMA_VERSION,
            network: ExecutionNetwork::Mainnet,
            account_address: config.account_address.clone(),
            integration_config_digest: config.digest().unwrap(),
            account_flat: true,
            health: OperationalHealth {
                observed_at_ms: at_ms,
                startup_reconciliation: Readiness::Ready,
                continuous_reconciliation: Readiness::Ready,
                unmanaged_orders: 0,
                unresolved_submissions: 0,
                rest_latency_ms: 10,
                private_latency_ms: 20,
                private_stream: Readiness::Ready,
                alerts: Readiness::Ready,
                audit_journal: Readiness::Ready,
                rollback: Readiness::Ready,
            },
            risk: RiskSnapshot {
                observed_at_ms: at_ms,
                market_data_at_ms: at_ms,
                reference_price: Decimal::new(80_000, 0),
                aggregate_notional: Decimal::ZERO,
                account_equity: Decimal::new(1_000, 0),
                open_order_count: 0,
                recent_order_times_ms: Vec::new(),
                rolling_pnl: Decimal::ZERO,
            },
        }
    }

    #[test]
    fn a_correctly_bound_envelope_round_trips_through_the_wire_format() {
        let config = runtime();
        let original = envelope(&config, 1_700_000_000_000);
        original.validate_binding(&config).unwrap();

        let decoded: HealthEnvelope =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();

        assert_eq!(decoded, original);
        decoded.validate_binding(&config).unwrap();
    }

    /// The gate reads health and risk together against one freshness bound, so
    /// two observation instants would let a stale half hide behind a fresh one.
    #[test]
    fn health_and_risk_must_describe_the_same_instant() {
        let config = runtime();
        let mut value = envelope(&config, 1_700_000_000_000);
        value.risk.observed_at_ms += 1;

        assert!(value.validate_binding(&config).is_err());
    }

    #[test]
    fn an_envelope_bound_to_another_account_or_config_is_refused() {
        let config = runtime();
        let at = 1_700_000_000_000;

        let mut wrong_account = envelope(&config, at);
        wrong_account.account_address = "0x9999999999999999999999999999999999999999".into();
        assert!(wrong_account.validate_binding(&config).is_err());

        let mut wrong_digest = envelope(&config, at);
        wrong_digest.integration_config_digest = "0".repeat(64);
        assert!(wrong_digest.validate_binding(&config).is_err());

        let mut wrong_network = envelope(&config, at);
        wrong_network.network = ExecutionNetwork::Testnet;
        assert!(wrong_network.validate_binding(&config).is_err());
    }

    /// An unknown field means the writer knows something this reader does not,
    /// which is not a difference to resolve by ignoring it.
    #[test]
    fn an_unknown_field_is_refused_rather_than_ignored() {
        let config = runtime();
        let mut value = serde_json::to_value(envelope(&config, 1_700_000_000_000)).unwrap();
        value["supervisor_note"] = "extra".into();

        assert!(serde_json::from_value::<HealthEnvelope>(value).is_err());
    }

    #[test]
    fn replacing_the_file_leaves_a_reader_a_complete_observation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("health.json");
        let config = runtime();

        envelope(&config, 1_700_000_000_000)
            .write_atomic(&path)
            .unwrap();
        let first: HealthEnvelope = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

        // Replacing an existing file must succeed and leave it readable.
        envelope(&config, 1_700_000_003_600)
            .write_atomic(&path)
            .unwrap();
        let second: HealthEnvelope =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

        assert_eq!(first.health.observed_at_ms, 1_700_000_000_000);
        assert_eq!(second.health.observed_at_ms, 1_700_000_003_600);
        assert!(!path.with_extension("tmp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_health_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("health.json");

        envelope(&runtime(), 1_700_000_000_000)
            .write_atomic(&path)
            .unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "health file is readable beyond its owner");
    }
}
