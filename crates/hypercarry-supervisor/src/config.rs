//! Supervisor settings, kept deliberately outside the reviewed digest.
//!
//! Nothing here may move into `RuntimeConfig`. That type's JSON serialization
//! *is* the reviewed configuration digest, so adding a supervisor knob to it
//! would change the digest of every existing configuration and invalidate every
//! release bundle reviewed against one. The supervisor reads the executor's
//! configuration to learn what it must attest, and keeps its own knobs here.

use anyhow::{Result, ensure};
use hypercarry_mainnet_config::validate_private_path;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Version of the supervisor configuration contract.
pub const SUPERVISOR_SCHEMA_VERSION: u32 = 1;

/// How the supervisor observes, and what it observes on behalf of.
#[derive(Debug, Clone, Deserialize)]
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
}

impl SupervisorConfig {
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
        Ok(config)
    }
}
