use anyhow::{Context, Result, bail, ensure};
use hypercarry_execution::{ExecutionNetwork, MarketMetadata, OrderIntent, RiskLimits, Side};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const CONFIG_SCHEMA_VERSION: u32 = 2;
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const EXPECTED_VENUE: &str = "hyperliquid";

/// Hyperliquid account balance model used to source live collateral.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AccountMode {
    Standard,
    UnifiedAccount,
}

impl AccountMode {
    pub(crate) const fn api_value(self) -> &'static str {
        match self {
            Self::Standard => "disabled",
            Self::UnifiedAccount => "unifiedAccount",
        }
    }
}

/// Validated, secret-free configuration for one testnet lifecycle.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    pub(crate) schema_version: u32,
    pub(crate) network: ExecutionNetwork,
    pub(crate) commit: String,
    pub(crate) session_id: String,
    pub(crate) account_address: String,
    pub(crate) authorized_signer_address: String,
    pub(crate) signer_alias: String,
    pub(crate) signer_socket: PathBuf,
    pub(crate) journal_path: PathBuf,
    pub(crate) kill_switch_path: PathBuf,
    pub(crate) evidence_path: PathBuf,
    pub(crate) market: MarketConfig,
    pub(crate) order: OrderConfig,
    pub(crate) risk: RiskConfig,
    pub(crate) transport: TransportConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MarketConfig {
    venue: String,
    name: String,
    asset_id: u32,
    #[serde(with = "rust_decimal::serde::str")]
    price_tick: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    size_step: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    minimum_size: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OrderConfig {
    correlation_id: String,
    side: Side,
    #[serde(with = "rust_decimal::serde::str")]
    quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    limit_price: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RiskConfig {
    account_mode: AccountMode,
    #[serde(with = "rust_decimal::serde::str")]
    max_order_notional: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    max_aggregate_notional: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    max_price_deviation_bps: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    max_leverage: Decimal,
    max_orders_per_window: usize,
    order_frequency_window_ms: i64,
    max_open_orders: usize,
    #[serde(with = "rust_decimal::serde::str")]
    max_rolling_loss: Decimal,
    max_market_data_age_ms: i64,
    snapshot_observed_at_ms: i64,
    market_data_at_ms: i64,
    #[serde(with = "rust_decimal::serde::str")]
    reference_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    aggregate_notional: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    account_equity: Decimal,
    open_order_count: usize,
    recent_order_times_ms: Vec<i64>,
    #[serde(with = "rust_decimal::serde::str")]
    rolling_pnl: Decimal,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransportConfig {
    connect_timeout_ms: u64,
    request_timeout_ms: u64,
    signer_timeout_ms: u64,
    action_ttl_ms: u64,
    max_reconcile_attempts: u32,
    reconcile_interval_ms: u64,
    max_requests_per_minute: usize,
    retry_max_attempts: u32,
    retry_base_backoff_ms: u64,
    retry_max_backoff_ms: u64,
}

/// Non-secret result of a successful local preflight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreflightSummary {
    /// Version of the configuration contract validated.
    pub schema_version: u32,
    /// Network the operator will target.
    pub network: ExecutionNetwork,
    /// Source commit recorded in the configuration.
    pub commit: String,
    /// Identifier of the planned session.
    pub session_id: String,
    /// Master trading account address.
    pub account_address: String,
    /// Authorized agent-wallet signer address.
    pub authorized_signer_address: String,
    /// Human-readable signer alias.
    pub signer_alias: String,
    /// Resolved account-mode label sourced from the official API.
    pub account_mode: String,
    /// Target market symbol.
    pub market: String,
    /// Buy or sell direction of the configured order.
    pub side: Side,
    /// Configured order quantity.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Configured limit price.
    #[serde(with = "rust_decimal::serde::str")]
    pub limit_price: Decimal,
    /// Whether the signer socket resolved to a loopback/local path.
    pub signer_socket_is_local: bool,
    /// Whether the filesystem kill switch is currently engaged.
    pub kill_switch_engaged: bool,
    /// SHA-256 digest of the validated configuration file.
    pub config_sha256: String,
}

impl OperatorConfig {
    pub(crate) fn read(path: &Path) -> Result<(Self, String)> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("cannot inspect config {}", path.display()))?;
        ensure!(
            metadata.file_type().is_file(),
            "config must be a regular file"
        );
        ensure!(
            metadata.len() <= MAX_CONFIG_BYTES,
            "config exceeds {MAX_CONFIG_BYTES} bytes"
        );
        let bytes = fs::read(path).context("cannot read operator config")?;
        let digest = hex_digest(&bytes);
        let config: Self =
            serde_json::from_slice(&bytes).context("operator config is not valid strict JSON")?;
        config.validate()?;
        Ok((config, digest))
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == CONFIG_SCHEMA_VERSION,
            "unsupported operator config schema version {}",
            self.schema_version
        );
        ensure!(
            self.network == ExecutionNetwork::Testnet,
            "operator config must use typed network testnet"
        );
        validate_hex("commit", &self.commit, 40)?;
        validate_identifier("session_id", &self.session_id, 128)?;
        validate_address("account_address", &self.account_address)?;
        validate_address("authorized_signer_address", &self.authorized_signer_address)?;
        ensure!(
            self.account_address != self.authorized_signer_address,
            "dedicated API wallet must differ from the trading account"
        );
        ensure!(
            self.commit.bytes().any(|byte| byte != b'0'),
            "commit must not use the all-zero placeholder"
        );
        validate_alias(&self.signer_alias)?;
        ensure!(
            self.market.venue == EXPECTED_VENUE,
            "venue must be exactly {EXPECTED_VENUE}"
        );
        ensure!(
            !self.market.name.trim().is_empty(),
            "market name must not be empty"
        );
        let metadata = self.market_metadata()?;
        let intent = self.order_intent(self.risk.snapshot_observed_at_ms)?;
        let _ = hypercarry_execution::ValidatedOrder::resolve(&intent, &metadata)?;
        self.risk_limits().validate()?;
        self.validate_snapshot_timestamps()?;
        self.validate_transport()?;
        self.validate_paths()?;
        Ok(())
    }

    pub(crate) fn preflight_summary(
        &self,
        config_sha256: String,
        kill_switch_engaged: bool,
    ) -> PreflightSummary {
        PreflightSummary {
            schema_version: CONFIG_SCHEMA_VERSION,
            network: self.network,
            commit: self.commit.clone(),
            session_id: self.session_id.clone(),
            account_address: self.account_address.clone(),
            authorized_signer_address: self.authorized_signer_address.clone(),
            signer_alias: self.signer_alias.clone(),
            account_mode: self.risk.account_mode.api_value().to_owned(),
            market: self.market.name.clone(),
            side: self.order.side,
            quantity: self.order.quantity,
            limit_price: self.order.limit_price,
            signer_socket_is_local: true,
            kill_switch_engaged,
            config_sha256,
        }
    }

    pub(crate) fn market_metadata(&self) -> Result<MarketMetadata> {
        Ok(MarketMetadata::new(
            &self.market.venue,
            &self.market.name,
            self.market.price_tick,
            self.market.size_step,
            self.market.minimum_size,
        )?)
    }

    pub(crate) fn order_intent(&self, created_at_ms: i64) -> Result<OrderIntent> {
        Ok(OrderIntent::limit(
            &self.order.correlation_id,
            &self.market.venue,
            &self.market.name,
            self.order.side,
            self.order.quantity,
            self.order.limit_price,
            created_at_ms,
        )?)
    }

    pub(crate) fn risk_limits(&self) -> RiskLimits {
        RiskLimits {
            allowed_markets: BTreeSet::from([format!(
                "{}:{}",
                self.market.venue, self.market.name
            )]),
            max_order_notional: self.risk.max_order_notional,
            max_aggregate_notional: self.risk.max_aggregate_notional,
            max_price_deviation_bps: self.risk.max_price_deviation_bps,
            max_leverage: self.risk.max_leverage,
            max_orders_per_window: self.risk.max_orders_per_window,
            order_frequency_window_ms: self.risk.order_frequency_window_ms,
            max_open_orders: self.risk.max_open_orders,
            max_rolling_loss: self.risk.max_rolling_loss,
            max_market_data_age_ms: self.risk.max_market_data_age_ms,
        }
    }

    pub(crate) const fn asset_id(&self) -> u32 {
        self.market.asset_id
    }

    pub(crate) const fn snapshot_observed_at_ms(&self) -> i64 {
        self.risk.snapshot_observed_at_ms
    }

    pub(crate) const fn market_data_at_ms(&self) -> i64 {
        self.risk.market_data_at_ms
    }

    pub(crate) const fn reference_price(&self) -> Decimal {
        self.risk.reference_price
    }

    pub(crate) const fn aggregate_notional(&self) -> Decimal {
        self.risk.aggregate_notional
    }

    pub(crate) const fn account_equity(&self) -> Decimal {
        self.risk.account_equity
    }

    pub(crate) const fn account_mode(&self) -> AccountMode {
        self.risk.account_mode
    }

    pub(crate) const fn open_order_count(&self) -> usize {
        self.risk.open_order_count
    }

    pub(crate) fn recent_order_times_ms(&self) -> Vec<i64> {
        self.risk.recent_order_times_ms.clone()
    }

    pub(crate) const fn rolling_pnl(&self) -> Decimal {
        self.risk.rolling_pnl
    }

    pub(crate) fn validate_snapshot_freshness(&self, now_ms: i64) -> Result<()> {
        let age = now_ms
            .checked_sub(self.risk.snapshot_observed_at_ms)
            .context("snapshot wall-clock age overflow")?;
        ensure!(age >= 0, "risk snapshot timestamp is in the future");
        ensure!(
            age <= self.risk.max_market_data_age_ms,
            "risk snapshot is stale relative to the operator clock"
        );
        Ok(())
    }

    pub(crate) const fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.transport.connect_timeout_ms)
    }

    pub(crate) const fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.transport.request_timeout_ms)
    }

    pub(crate) const fn signer_timeout(&self) -> Duration {
        Duration::from_millis(self.transport.signer_timeout_ms)
    }

    pub(crate) const fn action_ttl_ms(&self) -> u64 {
        self.transport.action_ttl_ms
    }

    pub(crate) const fn max_reconcile_attempts(&self) -> u32 {
        self.transport.max_reconcile_attempts
    }

    pub(crate) const fn reconcile_interval(&self) -> Duration {
        Duration::from_millis(self.transport.reconcile_interval_ms)
    }

    pub(crate) const fn max_requests_per_minute(&self) -> usize {
        self.transport.max_requests_per_minute
    }

    pub(crate) const fn retry_policy(&self) -> (u32, u64, u64) {
        (
            self.transport.retry_max_attempts,
            self.transport.retry_base_backoff_ms,
            self.transport.retry_max_backoff_ms,
        )
    }

    pub(crate) fn market_name(&self) -> &str {
        &self.market.name
    }

    pub(crate) const fn side(&self) -> Side {
        self.order.side
    }

    pub(crate) const fn quantity(&self) -> Decimal {
        self.order.quantity
    }

    pub(crate) const fn limit_price(&self) -> Decimal {
        self.order.limit_price
    }

    fn validate_snapshot_timestamps(&self) -> Result<()> {
        ensure!(
            self.risk.snapshot_observed_at_ms >= 0 && self.risk.market_data_at_ms >= 0,
            "snapshot timestamps must not be negative"
        );
        let age = self
            .risk
            .snapshot_observed_at_ms
            .checked_sub(self.risk.market_data_at_ms)
            .context("market-data age overflow")?;
        ensure!(age >= 0, "market data timestamp must not be in the future");
        ensure!(
            age <= self.risk.max_market_data_age_ms,
            "configured market data is already stale"
        );
        Ok(())
    }

    fn validate_transport(&self) -> Result<()> {
        for (name, value) in [
            ("connect_timeout_ms", self.transport.connect_timeout_ms),
            ("request_timeout_ms", self.transport.request_timeout_ms),
            ("signer_timeout_ms", self.transport.signer_timeout_ms),
            (
                "reconcile_interval_ms",
                self.transport.reconcile_interval_ms,
            ),
        ] {
            ensure!(
                (10..=60_000).contains(&value),
                "{name} must be in 10..=60000"
            );
        }
        ensure!(
            (1..=100).contains(&self.transport.max_reconcile_attempts),
            "max_reconcile_attempts must be in 1..=100"
        );
        ensure!(
            (3..=1_000).contains(&self.transport.max_requests_per_minute),
            "max_requests_per_minute must be in 3..=1000"
        );
        ensure!(
            (1..=60_000).contains(&self.transport.action_ttl_ms),
            "action_ttl_ms must be in 1..=60000"
        );
        ensure!(
            self.transport.retry_max_attempts > 0
                && self.transport.retry_base_backoff_ms > 0
                && self.transport.retry_max_backoff_ms >= self.transport.retry_base_backoff_ms,
            "retry policy must be positive and bounded"
        );
        Ok(())
    }

    fn validate_paths(&self) -> Result<()> {
        for (name, path) in [
            ("signer_socket", &self.signer_socket),
            ("journal_path", &self.journal_path),
            ("kill_switch_path", &self.kill_switch_path),
            ("evidence_path", &self.evidence_path),
        ] {
            ensure!(path.is_absolute(), "{name} must be an absolute path");
            let parent = path.parent().context("configured path has no parent")?;
            let metadata = fs::symlink_metadata(parent)
                .with_context(|| format!("cannot inspect {name} parent {}", parent.display()))?;
            ensure!(metadata.is_dir(), "{name} parent must be a directory");
            ensure!(
                !metadata.file_type().is_symlink(),
                "{name} parent must not be a symlink"
            );
            if let Ok(target) = fs::symlink_metadata(path) {
                ensure!(
                    !target.file_type().is_symlink(),
                    "{name} must not be a symlink"
                );
            }
        }
        ensure!(
            self.journal_path != self.kill_switch_path
                && self.journal_path != self.evidence_path
                && self.kill_switch_path != self.evidence_path,
            "journal, kill-switch, and evidence paths must be distinct"
        );
        ensure!(
            !self.evidence_path.exists(),
            "evidence path already exists; refusing to overwrite it"
        );
        Ok(())
    }
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_encode(&digest)
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len().saturating_mul(2)),
        |mut encoded, byte| {
            write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
            encoded
        },
    )
}

fn validate_address(name: &str, value: &str) -> Result<()> {
    ensure!(
        value.len() == 42
            && value.starts_with("0x")
            && value[2..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "{name} must be 0x plus 40 lowercase hexadecimal digits"
    );
    Ok(())
}

fn validate_hex(name: &str, value: &str, length: usize) -> Result<()> {
    ensure!(
        value.len() == length
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "{name} must contain exactly {length} lowercase hexadecimal characters"
    );
    Ok(())
}

fn validate_identifier(name: &str, value: &str, max: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > max
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("{name} must be a bounded non-secret identifier");
    }
    Ok(())
}

fn validate_alias(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
    {
        bail!("signer_alias must be a bounded non-secret provider alias");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tempfile::tempdir;

    #[test]
    fn config_is_testnet_only_strict_and_secret_free_by_schema() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("operator.json");
        let value = example_config(directory.path());
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let (config, digest) = OperatorConfig::read(&path).unwrap();
        assert_eq!(config.network, ExecutionNetwork::Testnet);
        assert_eq!(digest.len(), 64);

        let mut with_secret = value.clone();
        with_secret["private_key"] = Value::String("must-never-parse".to_owned());
        fs::write(&path, serde_json::to_vec(&with_secret).unwrap()).unwrap();
        assert!(OperatorConfig::read(&path).is_err());

        let mut mainnet = value;
        mainnet["network"] = Value::String("mainnet".to_owned());
        fs::write(&path, serde_json::to_vec(&mainnet).unwrap()).unwrap();
        assert!(OperatorConfig::read(&path).is_err());
    }

    #[test]
    fn stale_and_future_snapshots_fail_closed() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("operator.json");
        fs::write(
            &path,
            serde_json::to_vec(&example_config(directory.path())).unwrap(),
        )
        .unwrap();
        let (config, _) = OperatorConfig::read(&path).unwrap();
        assert!(
            config
                .validate_snapshot_freshness(config.snapshot_observed_at_ms() + 5_001)
                .is_err()
        );
        assert!(
            config
                .validate_snapshot_freshness(config.snapshot_observed_at_ms() - 1)
                .is_err()
        );
    }

    fn example_config(directory: &Path) -> Value {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        json!({
            "schema_version": 2,
            "network": "testnet",
            "commit": "a".repeat(40),
            "session_id": "session-1",
            "account_address": "0x1111111111111111111111111111111111111111",
            "authorized_signer_address": "0x2222222222222222222222222222222222222222",
            "signer_alias": "keychain/testnet-agent",
            "signer_socket": directory.join("signer.sock"),
            "journal_path": directory.join("journal.jsonl"),
            "kill_switch_path": directory.join("KILL"),
            "evidence_path": directory.join("evidence.json"),
            "market": {
                "venue": "hyperliquid",
                "name": "BTC",
                "asset_id": 0,
                "price_tick": "0.1",
                "size_step": "0.001",
                "minimum_size": "0.001"
            },
            "order": {
                "correlation_id": "operator-session-1",
                "side": "buy",
                "quantity": "0.001",
                "limit_price": "100"
            },
            "risk": {
                "account_mode": "unified_account",
                "max_order_notional": "1",
                "max_aggregate_notional": "2",
                "max_price_deviation_bps": "100",
                "max_leverage": "1",
                "max_orders_per_window": 1,
                "order_frequency_window_ms": 60000,
                "max_open_orders": 1,
                "max_rolling_loss": "0",
                "max_market_data_age_ms": 5000,
                "snapshot_observed_at_ms": now_ms,
                "market_data_at_ms": now_ms,
                "reference_price": "100",
                "aggregate_notional": "0",
                "account_equity": "100",
                "open_order_count": 0,
                "recent_order_times_ms": [],
                "rolling_pnl": "0"
            },
            "transport": {
                "connect_timeout_ms": 1000,
                "request_timeout_ms": 5000,
                "signer_timeout_ms": 1000,
                "action_ttl_ms": 5000,
                "max_reconcile_attempts": 5,
                "reconcile_interval_ms": 100,
                "max_requests_per_minute": 20,
                "retry_max_attempts": 3,
                "retry_base_backoff_ms": 100,
                "retry_max_backoff_ms": 1000
            }
        })
    }
}
