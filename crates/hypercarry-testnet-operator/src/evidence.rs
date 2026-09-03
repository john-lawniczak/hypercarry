use crate::config::hex_encode;
use anyhow::{Context, Result, ensure};
use hypercarry_execution::{ExecutionNetwork, OrderState, Side};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};
use tempfile::NamedTempFile;

const EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// Secret-free record produced by one bounded operator lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEvidence {
    /// Version of the evidence contract.
    pub schema_version: u32,
    /// Identifier of the completed session.
    pub session_id: String,
    /// Source commit the session ran at.
    pub commit: String,
    /// Session start time, unix milliseconds.
    pub started_at_ms: i64,
    /// Session end time, unix milliseconds.
    pub ended_at_ms: i64,
    /// Network the session targeted.
    pub network: ExecutionNetwork,
    /// Master trading account address.
    pub account_address: String,
    /// Authorized agent-wallet signer address.
    pub authorized_signer_address: String,
    /// Human-readable signer alias.
    pub signer_alias: String,
    /// Target market symbol.
    pub market: String,
    /// Buy or sell direction of the order.
    pub side: Side,
    /// Requested order quantity.
    #[serde(with = "rust_decimal::serde::str")]
    pub requested_quantity: Decimal,
    /// Requested limit price.
    #[serde(with = "rust_decimal::serde::str")]
    pub requested_limit_price: Decimal,
    /// Durable client order identifier used for the session.
    pub client_order_id: String,
    /// Final observed lifecycle state.
    pub final_state: OrderState,
    /// Cumulative filled quantity at session end.
    #[serde(with = "rust_decimal::serde::str")]
    pub cumulative_filled: Decimal,
    /// Independent account-wide open-order count observed at close.
    pub independent_open_order_count: usize,
    /// Submissions whose outcome was never resolved.
    pub unresolved_submission_count: u32,
    /// Times a risk policy bypass was observed.
    pub policy_bypass_count: u32,
    /// Whether the session ended with no orphaned or unmanaged state.
    pub lifecycle_clean: bool,
    /// SHA-256 digest of the operator configuration used.
    pub config_sha256: String,
    /// SHA-256 digest of the operator executable used.
    pub operator_executable_sha256: String,
    /// SHA-256 digest of the durable journal produced.
    pub journal_sha256: String,
    /// Names of fault-injection exercises performed during the session.
    pub faults_injected: Vec<String>,
    /// Whether a manual secret scan of the artifacts was completed.
    pub manual_secret_scan_completed: bool,
    /// Independent human reviewer identity, once attested.
    pub reviewer: Option<String>,
}

pub(crate) struct EvidenceInput {
    pub session_id: String,
    pub commit: String,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub account_address: String,
    pub authorized_signer_address: String,
    pub signer_alias: String,
    pub market: String,
    pub side: Side,
    pub requested_quantity: Decimal,
    pub requested_limit_price: Decimal,
    pub client_order_id: String,
    pub final_state: OrderState,
    pub cumulative_filled: Decimal,
    pub independent_open_order_count: usize,
    pub unresolved_submission_count: u32,
    pub config_sha256: String,
    pub operator_executable_sha256: String,
    pub journal_sha256: String,
}

impl SessionEvidence {
    pub(crate) fn from_input(input: EvidenceInput) -> Self {
        let lifecycle_clean = matches!(
            input.final_state,
            OrderState::Cancelled | OrderState::Filled
        ) && input.independent_open_order_count == 0
            && input.unresolved_submission_count == 0;
        Self {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            session_id: input.session_id,
            commit: input.commit,
            started_at_ms: input.started_at_ms,
            ended_at_ms: input.ended_at_ms,
            network: ExecutionNetwork::Testnet,
            account_address: input.account_address,
            authorized_signer_address: input.authorized_signer_address,
            signer_alias: input.signer_alias,
            market: input.market,
            side: input.side,
            requested_quantity: input.requested_quantity,
            requested_limit_price: input.requested_limit_price,
            client_order_id: input.client_order_id,
            final_state: input.final_state,
            cumulative_filled: input.cumulative_filled,
            independent_open_order_count: input.independent_open_order_count,
            unresolved_submission_count: input.unresolved_submission_count,
            policy_bypass_count: 0,
            lifecycle_clean,
            config_sha256: input.config_sha256,
            operator_executable_sha256: input.operator_executable_sha256,
            journal_sha256: input.journal_sha256,
            faults_injected: Vec::new(),
            manual_secret_scan_completed: false,
            reviewer: None,
        }
    }

    pub(crate) fn write_new(&self, path: &Path) -> Result<()> {
        ensure!(!path.exists(), "refusing to overwrite existing evidence");
        let parent = path.parent().context("evidence path has no parent")?;
        let mut temporary =
            NamedTempFile::new_in(parent).context("could not create temporary evidence file")?;
        serde_json::to_writer_pretty(&mut temporary, self)
            .context("could not serialize session evidence")?;
        temporary
            .write_all(b"\n")
            .context("could not terminate session evidence")?;
        temporary
            .as_file()
            .sync_all()
            .context("could not sync session evidence")?;
        temporary
            .persist_noclobber(path)
            .map_err(|error| error.error)
            .context("could not atomically persist session evidence")?;
        sync_directory(parent)?;
        Ok(())
    }
}

pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).context("could not open journal for hashing")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .context("could not hash durable journal")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    Ok(hex_encode(&digest))
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("could not sync evidence directory {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn evidence() -> SessionEvidence {
        SessionEvidence::from_input(EvidenceInput {
            session_id: "session-1".to_owned(),
            commit: "a".repeat(40),
            started_at_ms: 1,
            ended_at_ms: 2,
            account_address: "0x1111111111111111111111111111111111111111".to_owned(),
            authorized_signer_address: "0x2222222222222222222222222222222222222222".to_owned(),
            signer_alias: "keychain/testnet".to_owned(),
            market: "BTC".to_owned(),
            side: Side::Buy,
            requested_quantity: Decimal::ONE,
            requested_limit_price: Decimal::ONE,
            client_order_id: format!("0x{}", "1".repeat(32)),
            final_state: OrderState::Cancelled,
            cumulative_filled: Decimal::ZERO,
            independent_open_order_count: 0,
            unresolved_submission_count: 0,
            config_sha256: "b".repeat(64),
            operator_executable_sha256: "d".repeat(64),
            journal_sha256: "c".repeat(64),
        })
    }

    #[test]
    fn evidence_is_durable_and_never_overwritten() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("evidence.json");
        evidence().write_new(&path).unwrap();
        assert!(evidence().write_new(&path).is_err());
        let parsed: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(parsed["lifecycle_clean"], true);
        assert_eq!(parsed["manual_secret_scan_completed"], false);
        assert!(parsed.get("signature").is_none());
    }
}
