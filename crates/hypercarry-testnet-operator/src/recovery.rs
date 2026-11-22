//! Local testnet recovery host. The signer is separately started and scoped;
//! this process neither loads a key nor opens/adopts a manual position.
use crate::{OPERATOR_ACKNOWLEDGEMENT, signer::UnixSocketTestnetSigner};
use anyhow::{Result, ensure};
use hypercarry_execution::{
    ExecutionNetwork, FileJournal, Journal,
    bracket::BracketManager,
    read_journal,
    recovery::{RecoveryLimits, RecoveryManager, RecoveryPhase, RecoveryState, RecoveryVenue},
};
use hypercarry_hyperliquid::{
    HyperliquidTestnetConfig, ReqwestHyperliquidTransport, SystemClock, TESTNET_ACKNOWLEDGEMENT,
    TestnetAcknowledgement, TestnetRecoveryAdapter,
};
use hypercarry_notifications::{private_dir, read_private};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Private local configuration; no secrets, recipients or arbitrary endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryConfig {
    /// Must be one and testnet. Mainnet is structurally refused by the adapter.
    pub schema_version: u32,
    /// Explicit network; never inferred from an endpoint.
    pub network: ExecutionNetwork,
    /// Existing journal of the owned entry, pinned by its specification digest.
    pub bracket_journal: PathBuf,
    /// Digest displayed when the original bracket was reviewed.
    pub bracket_spec_digest: String,
    /// Shared private directory for account lock, nonce state, entry gate and journal.
    pub operations_dir: PathBuf,
    /// Native or builder-perpetual asset ID, rechecked against metadata every pass.
    pub asset_id: u32,
    /// Dedicated approved agent; never the owner address.
    pub signer_address: String,
    /// Public, pinned signer alias.
    pub signer_alias: String,
    /// Private socket of a compatible separately scoped testnet signer.
    pub signer_socket: PathBuf,
    /// Time limit for the external signer, including interactive unlock if any.
    pub signer_timeout_ms: u64,
    /// REST polling interval; conservative lower bound for this full-snapshot adapter.
    pub poll_ms: u64,
    /// Bounded session duration. Native exits and the closed entry gate survive exit.
    pub session_ms: u64,
    /// Exact fallback price, quantity precision and retry bounds.
    pub limits: RecoveryLimits,
}
impl RecoveryConfig {
    /// # Errors
    /// Rejects mainnet, unsafe paths, unbounded sessions and changed trade identity.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1 && self.network == ExecutionNetwork::Testnet,
            "recovery host is testnet-only"
        );
        self.limits.validate()?;
        ensure!(
            self.bracket_spec_digest.len() == 64
                && self
                    .bracket_spec_digest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit()),
            "invalid bracket digest"
        );
        ensure!(
            self.bracket_journal.is_absolute() && self.signer_socket.is_absolute(),
            "absolute local paths required"
        );
        ensure!(
            (15_000..=60_000).contains(&self.poll_ms)
                && (self.poll_ms..=3_600_000).contains(&self.session_ms),
            "invalid polling/session bounds"
        );
        ensure!(
            (100..=5000).contains(&self.signer_timeout_ms)
                && self.signer_timeout_ms < self.limits.action_ttl_ms,
            "signer timeout must fit action TTL"
        );
        private_dir(&self.operations_dir)?;
        Ok(())
    }
}
fn now() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
/// Execute one or a bounded series of testnet recovery passes. Requires an
/// already-owned bracket and an independently scoped local signer. Never sends
/// email/SMS inline; the worker tails the resulting journal separately.
/// # Errors
/// Rejects invalid acknowledgement/configuration, missing ownership, journal
/// drift, signer/socket failures and unsafe runtime boundaries.
pub fn run_recovery(path: &Path, acknowledgement: &str, once: bool) -> Result<RecoveryState> {
    ensure!(
        acknowledgement == OPERATOR_ACKNOWLEDGEMENT,
        "explicit testnet external-effect acknowledgement required"
    );
    let config: RecoveryConfig = serde_json::from_slice(&read_private(path)?)?;
    config.validate()?;
    read_private(&config.bracket_journal)?;
    let _bracket_lock = FileJournal::open(&config.bracket_journal)?;
    let bracket = BracketManager::replay(&read_journal(&config.bracket_journal)?)?;
    ensure!(
        bracket.spec.network == ExecutionNetwork::Testnet
            && bracket.spec.digest()? == config.bracket_spec_digest,
        "bracket scope/digest mismatch"
    );
    let venue_config = HyperliquidTestnetConfig::new(
        bracket.spec.account.clone(),
        config.limits.action_ttl_ms,
        TestnetAcknowledgement::new(TESTNET_ACKNOWLEDGEMENT)?,
    )?
    .with_authorized_signer_address(config.signer_address.clone())?;
    let signer = UnixSocketTestnetSigner::new(
        config.signer_socket.clone(),
        config.signer_alias.clone(),
        config.signer_address.clone(),
        Duration::from_millis(config.signer_timeout_ms),
    )?;
    let transport =
        ReqwestHyperliquidTransport::new(Duration::from_secs(2), Duration::from_secs(3))?;
    let policy = hypercarry_execution::recovery::RecoveryPolicy::from_bracket(
        &bracket,
        config.limits.clone(),
    )?;
    let mut adapter = TestnetRecoveryAdapter::new(
        policy.clone(),
        venue_config,
        config.asset_id,
        transport,
        signer,
        SystemClock,
        &config.operations_dir,
    )?;
    adapter.freeze_entries()?;
    let journal_path = config.operations_dir.join(format!(
        "testnet-{}-{}.recovery.jsonl",
        bracket.spec.account, bracket.spec.trade_id
    ));
    if journal_path.symlink_metadata().is_ok() {
        read_private(&journal_path)?;
    }
    let mut journal = FileJournal::open(&journal_path)?;
    let mut manager = if journal.next_sequence() == Some(0) {
        RecoveryManager::from_bracket(&bracket, config.limits.clone(), &mut journal, now()?)?
    } else {
        let m = RecoveryManager::replay(&read_journal(&journal_path)?)?;
        ensure!(
            m.policy() == &policy,
            "recovery policy changed; explicit review required"
        );
        m
    };
    let interrupted = Arc::new(AtomicBool::new(false));
    let signals = [
        signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())?,
        signal_hook::flag::register(signal_hook::consts::SIGTERM, interrupted.clone())?,
    ];
    let started = Instant::now();
    let result = (|| -> Result<RecoveryState> {
        loop {
            if interrupted.load(Ordering::Relaxed) {
                break;
            }
            manager.tick(&mut journal, &mut adapter)?;
            if once
                || matches!(
                    manager.state().phase,
                    RecoveryPhase::Closed | RecoveryPhase::Escalated
                )
                || started.elapsed() >= Duration::from_millis(config.session_ms)
            {
                break;
            }
            let until = Instant::now() + Duration::from_millis(config.poll_ms);
            while Instant::now() < until
                && !interrupted.load(Ordering::Relaxed)
                && started.elapsed() < Duration::from_millis(config.session_ms)
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            if started.elapsed() >= Duration::from_millis(config.session_ms) {
                break;
            }
        }
        Ok(manager.state().clone())
    })();
    for id in signals {
        signal_hook::low_level::unregister(id);
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_acknowledgement_is_rejected_before_file_or_socket_access() {
        assert!(
            run_recovery(Path::new("/does/not/exist"), "yes", true)
                .unwrap_err()
                .to_string()
                .contains("acknowledgement")
        );
    }
}
