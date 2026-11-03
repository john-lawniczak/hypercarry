//! Supervisor settings, bound transitively through an optional runtime digest.
//!
//! Historical runtime digests remain unchanged when the optional supervisor
//! policy digest is absent. The daemon requires that binding before it can
//! publish readiness; changing a probe, cadence or observation window requires
//! new review. The supervisor never links the signing stack.

use anyhow::{Result, ensure};
use hypercarry_mainnet_config::validate_private_path;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Version of the supervisor configuration contract.
pub const SUPERVISOR_SCHEMA_VERSION: u32 = 1;

/// How the supervisor observes, and what it observes on behalf of.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorConfig {
    /// Version of this contract.
    pub schema_version: u32,
    /// The executor configuration this supervisor attests for.
    ///
    /// Read, never written. The supervisor takes the account address, the
    /// health-file destination and the reviewed digest from it, so the two
    /// processes cannot disagree about which account is being supervised.
    pub executor_config: PathBuf,
    /// Trailing window over which realized `PnL` is summed, milliseconds.
    ///
    /// The risk limits bound the trailing loss but do not say over what period,
    /// because the period is an observation choice rather than a policy one.
    pub rolling_pnl_window_ms: i64,
    /// Interval between observations, milliseconds.
    pub sample_interval_ms: u64,
    /// Hash-pinned executable that checks actual alert delivery and recovery
    /// artifact availability. Absent means these fields remain not ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operational_probe: Option<ProbeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeConfig {
    pub executable: PathBuf,
    pub sha256: String,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_sha256: Option<String>,
}

impl SupervisorConfig {
    /// Validate output/input separation and the supervised runtime.
    /// # Errors
    /// Rejects unsafe paths, aliases, identities and invalid order/risk limits.
    pub fn validate_runtime(
        &self,
        runtime: &hypercarry_mainnet_config::RuntimeConfig,
        source: &Path,
    ) -> Result<()> {
        let lock = runtime.health_file.with_extension("supervisor.lock");
        let paths = [
            &lock,
            &runtime.health_file,
            &runtime.journal,
            &runtime.signer_socket,
            &runtime.heartbeat,
            &runtime.kill_switch,
        ];
        hypercarry_mainnet_config::validate_disjoint_paths(
            paths
                .iter()
                .map(|p| p.as_path())
                .chain([source, self.executor_config.as_path()])
                .chain(runtime.executor_lease.as_deref())
                .chain(
                    self.operational_probe
                        .as_ref()
                        .map(|p| p.executable.as_path()),
                )
                .chain(
                    self.operational_probe
                        .as_ref()
                        .and_then(|p| p.config.as_deref()),
                ),
        )?;
        for (index, path) in paths.iter().enumerate() {
            ensure!(
                path.is_absolute() && !paths[..index].contains(path),
                "runtime paths must be distinct and absolute"
            );
            validate_private_path(path)?;
            let parent = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("runtime parent missing"))?;
            ensure!(
                std::fs::canonicalize(parent)? == parent,
                "runtime parent must be canonical"
            );
        }
        // Health withdrawal/publication must never delete an authority input.
        for input in [
            Some(source),
            Some(self.executor_config.as_path()),
            runtime.executor_lease.as_deref(),
            self.operational_probe
                .as_ref()
                .map(|probe| probe.executable.as_path()),
        ]
        .into_iter()
        .flatten()
        {
            for output in [&runtime.health_file, &lock] {
                ensure!(
                    input != output && std::fs::canonicalize(input).ok().as_ref() != Some(output),
                    "supervisor output overlaps authority input"
                );
            }
        }
        ensure!(
            runtime.account_address.len() == 42
                && runtime.account_address.starts_with("0x")
                && runtime.account_address[2..]
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit()),
            "invalid supervised account"
        );
        runtime.order.validate()?;
        runtime.risk.validate()?;
        Ok(())
    }

    /// Compute the canonical supervisor-policy digest.
    /// # Errors
    /// Returns an error if configuration serialization fails.
    pub fn digest(&self) -> Result<String> {
        Ok(hex(&Sha256::digest(serde_json::to_vec(self)?)))
    }
    /// Reads and validates a supervisor configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for an unreadable or malformed file, an unsupported
    /// schema version, a bound outside its permitted range, or an executor
    /// configuration path that is not absolute and privately held.
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = hypercarry_mainnet_config::read_json(path)?;
        ensure!(
            config.schema_version == SUPERVISOR_SCHEMA_VERSION,
            "unsupported supervisor schema"
        );
        ensure!(
            config.executor_config.is_absolute(),
            "executor config path must be absolute"
        );
        validate_private_path(&config.executor_config)?;
        // A window shorter than a minute cannot capture a trailing loss; one
        // longer than a week stops describing current exposure.
        ensure!(
            (60_000..=604_800_000).contains(&config.rolling_pnl_window_ms),
            "rolling PnL window must be 1 minute to 7 days"
        );
        // The executor treats health older than its own bound as stale, so
        // sampling far slower than that guarantees it never sees a fresh file.
        ensure!(
            (1_000..=60_000).contains(&config.sample_interval_ms),
            "sample interval must be 1s to 60s"
        );
        if let Some(probe) = &config.operational_probe {
            ensure!(
                probe.config.is_some() == probe.config_sha256.is_some(),
                "probe config and digest must be supplied together"
            );
            if let Some(path) = &probe.config {
                ensure!(path.is_absolute(), "probe config must be absolute");
                validate_private_path(path)?;
                ensure!(
                    probe
                        .config_sha256
                        .as_ref()
                        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())),
                    "invalid probe config digest"
                );
            }
            ensure!(
                probe.executable.is_absolute() && (100..=10_000).contains(&probe.timeout_ms),
                "invalid operational probe path or timeout"
            );
            ensure!(
                probe.sha256.len() == 64 && probe.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid operational probe digest"
            );
        }
        Ok(config)
    }
}

pub fn hex(bytes: &[u8]) -> String {
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
mod tests {
    use super::*;
    #[test]
    fn supervisor_refuses_health_paths_that_can_destroy_state_or_config() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut runtime: hypercarry_mainnet_config::RuntimeConfig = serde_json::from_str(
            include_str!("../../../docs/mainnet-runtime-config.example.json"),
        )
        .unwrap();
        runtime.journal = root.join("journal");
        runtime.health_file = root.join("health");
        runtime.signer_socket = root.join("socket");
        runtime.heartbeat = root.join("heartbeat");
        runtime.kill_switch = root.join("STOP");
        let supervisor = SupervisorConfig {
            schema_version: 1,
            executor_config: root.join("runtime"),
            rolling_pnl_window_ms: 60_000,
            sample_interval_ms: 1000,
            operational_probe: None,
        };
        let source = root.join("supervisor");
        assert!(supervisor.validate_runtime(&runtime, &source).is_ok());
        runtime.health_file = runtime.journal.clone();
        assert!(supervisor.validate_runtime(&runtime, &source).is_err());
        runtime.health_file = supervisor.executor_config.clone();
        assert!(supervisor.validate_runtime(&runtime, &source).is_err());
        runtime.health_file = root.join("health");
        let lock = runtime.health_file.with_extension("supervisor.lock");
        runtime.journal = lock.clone();
        assert!(supervisor.validate_runtime(&runtime, &source).is_err());
        runtime.journal = root.join("journal");
        runtime.executor_lease = Some(lock.clone());
        assert!(supervisor.validate_runtime(&runtime, &source).is_err());
        runtime.executor_lease = None;
        assert!(supervisor.validate_runtime(&runtime, &lock).is_err());
        let mut aliased = supervisor.clone();
        aliased.executor_config = lock;
        assert!(aliased.validate_runtime(&runtime, &source).is_err());
    }
}
