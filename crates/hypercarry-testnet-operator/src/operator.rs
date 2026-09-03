use crate::{
    config::{AccountMode, OperatorConfig, PreflightSummary},
    evidence::{EvidenceInput, SessionEvidence, hash_file},
    signer::{UnixSocketTestnetSigner, validate_socket},
};
use anyhow::{Context, Result, bail, ensure};
use hypercarry_execution::{
    ComprehensiveRiskPolicy, ExecutionError, FileJournal, FileKillSwitch, HyperliquidAssetResolver,
    HyperliquidTestnetConfig, HyperliquidTestnetExecutor, HyperliquidTransport, KillSwitch,
    MarketMetadata, MarketMetadataResolver, OrderState, RequestThrottle,
    ReqwestHyperliquidTransport, RetryPolicy, RiskSnapshot, RiskSnapshotSource, SystemClock,
    TESTNET_ACKNOWLEDGEMENT, TestnetAcknowledgement, TestnetReliability, ValidatedOrder,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{
    path::Path,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

type OperatorExecutor = HyperliquidTestnetExecutor<
    UnixSocketTestnetSigner,
    ReqwestHyperliquidTransport,
    FileJournal,
    SystemClock,
    ConfiguredMarket,
>;

type OperatorPolicy = ComprehensiveRiskPolicy<ConfiguredSnapshot, FileKillSwitch>;

/// Result category for one evidence-producing lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorOutcome {
    /// Terminal lifecycle with no independently observed open orders.
    Clean(SessionEvidence),
    /// Nonterminal lifecycle or independently observed open orders.
    Unclean(SessionEvidence),
}

impl OperatorOutcome {
    /// Returns the secret-free evidence for either outcome.
    pub const fn evidence(&self) -> &SessionEvidence {
        match self {
            Self::Clean(evidence) | Self::Unclean(evidence) => evidence,
        }
    }

    /// Reports whether the lifecycle itself is clean.
    pub const fn is_clean(&self) -> bool {
        matches!(self, Self::Clean(_))
    }
}

/// Validates config, filesystem boundaries, socket permissions, freshness, and
/// kill-switch state without network I/O or signing.
///
/// # Errors
///
/// Returns an error for any malformed, stale, ambiguous, or inaccessible
/// boundary.
pub fn preflight(config_path: &Path) -> Result<PreflightSummary> {
    let (config, digest) = OperatorConfig::read(config_path)?;
    validate_socket(&config.signer_socket).context("signer socket preflight failed")?;
    let now_ms = now_ms()?;
    config.validate_snapshot_freshness(now_ms)?;
    let kill_switch = FileKillSwitch::new(&config.kill_switch_path);
    let engaged = kill_switch
        .is_engaged()
        .context("could not establish kill-switch state")?;
    Ok(config.preflight_summary(digest, engaged))
}

/// Places, cancels, and REST-reconciles one exact order against the fixed
/// official Hyperliquid testnet endpoint, then writes non-overwriting evidence.
///
/// # Errors
///
/// Returns an error before signing when preflight or risk state is invalid, or
/// when a transport, signer, journal, reconciliation, or evidence boundary
/// fails. The durable journal remains available for recovery after errors.
pub fn run_session(config_path: &Path) -> Result<OperatorOutcome> {
    let (config, config_sha256) = OperatorConfig::read(config_path)?;
    validate_socket(&config.signer_socket).context("signer socket preflight failed")?;
    let started_at_ms = now_ms()?;
    config.validate_snapshot_freshness(started_at_ms)?;
    let kill_switch = FileKillSwitch::new(&config.kill_switch_path);
    ensure!(
        !kill_switch
            .is_engaged()
            .context("could not establish kill-switch state")?,
        "kill switch is engaged"
    );

    verify_live_account_state(&config)?;

    let (mut executor, resolver, policy) = build_executor(&config)?;
    let intent = config.order_intent(started_at_ms)?;
    let live = execute_lifecycle(&config, &mut executor, &resolver, &policy, &intent)?;
    finalize_session(&config, config_sha256, started_at_ms, executor, &live)
}

fn verify_live_account_state(config: &OperatorConfig) -> Result<()> {
    let mut transport =
        ReqwestHyperliquidTransport::new(config.connect_timeout(), config.request_timeout())?;
    let account = &config.account_address;

    let abstraction = transport
        .info(&json!({"type": "userAbstraction", "user": account}))
        .map_err(|_| anyhow::anyhow!("live account-mode query failed"))?;
    ensure!(
        abstraction.as_str() == Some(config.account_mode().api_value()),
        "live account mode does not match the reviewed configuration"
    );

    let collateral_response = match config.account_mode() {
        AccountMode::Standard => transport
            .info(&json!({"type": "clearinghouseState", "user": account}))
            .map_err(|_| anyhow::anyhow!("live standard-account collateral query failed"))?,
        AccountMode::UnifiedAccount => transport
            .info(&json!({"type": "spotClearinghouseState", "user": account}))
            .map_err(|_| anyhow::anyhow!("live unified-account collateral query failed"))?,
    };
    let live_equity = parse_available_equity(config.account_mode(), &collateral_response)?;
    ensure!(
        live_equity >= config.account_equity(),
        "live available collateral is below the reviewed risk snapshot"
    );

    let orders = transport
        .info(&json!({"type": "openOrders", "user": account}))
        .map_err(|_| anyhow::anyhow!("live open-orders preflight query failed"))?;
    let live_open_orders = orders
        .as_array()
        .context("live open-orders response must be an array")?;
    ensure!(
        live_open_orders.iter().all(Value::is_object),
        "live open-orders response contains a non-object"
    );
    ensure!(
        live_open_orders.len() == config.open_order_count(),
        "live open-order count does not match the reviewed risk snapshot"
    );

    let role = transport
        .info(&json!({
            "type": "userRole",
            "user": config.authorized_signer_address
        }))
        .map_err(|_| anyhow::anyhow!("live agent-authorization query failed"))?;
    validate_agent_role(&role, account)
}

fn parse_available_equity(mode: AccountMode, response: &Value) -> Result<Decimal> {
    let text = match mode {
        AccountMode::Standard => response
            .pointer("/marginSummary/accountValue")
            .and_then(Value::as_str)
            .context("standard account response lacks margin account value")?,
        AccountMode::UnifiedAccount => response
            .get("tokenToAvailableAfterMaintenance")
            .and_then(Value::as_array)
            .context("unified account response lacks available token balances")?
            .iter()
            .find_map(|entry| {
                let pair = entry.as_array()?;
                (pair.len() == 2 && pair[0].as_u64() == Some(0))
                    .then(|| pair[1].as_str())
                    .flatten()
            })
            .context("unified account response lacks available USDC")?,
    };
    let equity = text
        .parse::<Decimal>()
        .context("live collateral is not a valid decimal")?;
    ensure!(
        equity > Decimal::ZERO,
        "live available collateral is not positive"
    );
    Ok(equity)
}

fn validate_agent_role(response: &Value, expected_master: &str) -> Result<()> {
    ensure!(
        response.get("role").and_then(Value::as_str) == Some("agent"),
        "configured signer is not an authorized agent"
    );
    let master = response
        .pointer("/data/user")
        .and_then(Value::as_str)
        .context("agent role response lacks its master account")?;
    ensure!(
        master.eq_ignore_ascii_case(expected_master),
        "configured agent is authorized for a different master account"
    );
    Ok(())
}

fn build_executor(
    config: &OperatorConfig,
) -> Result<(OperatorExecutor, ConfiguredMarket, OperatorPolicy)> {
    let kill_switch = FileKillSwitch::new(&config.kill_switch_path);
    let signer = UnixSocketTestnetSigner::new(
        config.signer_socket.clone(),
        config.signer_alias.clone(),
        config.authorized_signer_address.clone(),
        config.signer_timeout(),
    )
    .context("could not construct pinned external signer")?;
    let acknowledgement = TestnetAcknowledgement::new(TESTNET_ACKNOWLEDGEMENT)?;
    let executor_config = HyperliquidTestnetConfig::new(
        &config.account_address,
        config.action_ttl_ms(),
        acknowledgement,
    )?
    .with_authorized_signer_address(&config.authorized_signer_address)?;
    let transport =
        ReqwestHyperliquidTransport::new(config.connect_timeout(), config.request_timeout())?;
    let journal = FileJournal::open(&config.journal_path)?;
    let throttle = RequestThrottle::new(config.max_requests_per_minute(), 60_000)?;
    let (attempts, base_backoff, max_backoff) = config.retry_policy();
    let retry = RetryPolicy::new(attempts, base_backoff, max_backoff)?;
    let reliability = TestnetReliability::new(throttle, retry);
    let resolver = ConfiguredMarket {
        metadata: config.market_metadata()?,
        asset_id: config.asset_id(),
    };
    let snapshot = ConfiguredSnapshot(RiskSnapshot {
        observed_at_ms: config.snapshot_observed_at_ms(),
        market_data_at_ms: config.market_data_at_ms(),
        reference_price: config.reference_price(),
        aggregate_notional: config.aggregate_notional(),
        account_equity: config.account_equity(),
        open_order_count: config.open_order_count(),
        recent_order_times_ms: config.recent_order_times_ms(),
        rolling_pnl: config.rolling_pnl(),
    });
    let policy = ComprehensiveRiskPolicy::new(config.risk_limits(), snapshot, kill_switch)?;
    let executor = HyperliquidTestnetExecutor::new(
        executor_config,
        signer,
        transport,
        journal,
        SystemClock,
        resolver.clone(),
        reliability,
    )?;
    Ok((executor, resolver, policy))
}

fn execute_lifecycle(
    config: &OperatorConfig,
    executor: &mut OperatorExecutor,
    resolver: &ConfiguredMarket,
    policy: &OperatorPolicy,
    intent: &hypercarry_execution::OrderIntent,
) -> Result<hypercarry_execution::LiveOrder> {
    let mut live = executor.place(intent, resolver, policy)?;
    reconcile_while(
        executor,
        &mut live,
        config.max_reconcile_attempts(),
        config.reconcile_interval(),
        |state| state == OrderState::SubmissionUncertain,
    )?;
    match live.state() {
        OrderState::Open | OrderState::PartiallyFilled => executor.cancel(&mut live)?,
        // Do not sign a cancellation while placement existence remains
        // unresolved. The independent account query still runs at finalization.
        OrderState::Filled
        | OrderState::Cancelled
        | OrderState::Rejected
        | OrderState::SubmissionUncertain => {}
        state => bail!("executor returned unsupported post-placement state {state:?}"),
    }
    reconcile_while(
        executor,
        &mut live,
        config.max_reconcile_attempts(),
        config.reconcile_interval(),
        |state| !is_terminal(state),
    )?;
    Ok(live)
}

fn finalize_session(
    config: &OperatorConfig,
    config_sha256: String,
    started_at_ms: i64,
    executor: OperatorExecutor,
    live: &hypercarry_execution::LiveOrder,
) -> Result<OperatorOutcome> {
    let final_state = live.state();
    let cumulative_filled = live.cumulative_filled();
    let client_order_id = live.client_order_id().to_string();
    let unresolved_submission_count = u32::from(matches!(
        final_state,
        OrderState::SubmissionPending | OrderState::SubmissionUncertain
    ));
    let (mut transport, journal) = executor.into_parts();
    let independent_open_order_count =
        independent_open_order_count(&mut transport, &config.account_address)?;
    drop(transport);
    drop(journal);
    let journal_sha256 = hash_file(&config.journal_path)?;
    let executable = std::env::current_exe().context("cannot resolve operator executable")?;
    let operator_executable_sha256 = hash_file(&executable)?;
    let ended_at_ms = now_ms()?;
    let evidence = SessionEvidence::from_input(EvidenceInput {
        session_id: config.session_id.clone(),
        commit: config.commit.clone(),
        started_at_ms,
        ended_at_ms,
        account_address: config.account_address.clone(),
        authorized_signer_address: config.authorized_signer_address.clone(),
        signer_alias: config.signer_alias.clone(),
        market: config.market_name().to_owned(),
        side: config.side(),
        requested_quantity: config.quantity(),
        requested_limit_price: config.limit_price(),
        client_order_id,
        final_state,
        cumulative_filled,
        independent_open_order_count,
        unresolved_submission_count,
        config_sha256,
        operator_executable_sha256,
        journal_sha256,
    });
    evidence.write_new(&config.evidence_path)?;
    if evidence.lifecycle_clean {
        Ok(OperatorOutcome::Clean(evidence))
    } else {
        Ok(OperatorOutcome::Unclean(evidence))
    }
}

fn reconcile_while<S, T, J, C, A, P>(
    executor: &mut HyperliquidTestnetExecutor<S, T, J, C, A>,
    live: &mut hypercarry_execution::LiveOrder,
    max_attempts: u32,
    interval: std::time::Duration,
    predicate: P,
) -> Result<()>
where
    S: hypercarry_execution::HyperliquidL1Signer,
    T: HyperliquidTransport,
    J: hypercarry_execution::Journal,
    C: hypercarry_execution::Clock,
    A: HyperliquidAssetResolver,
    P: Fn(OrderState) -> bool,
{
    for attempt in 0..max_attempts {
        if !predicate(live.state()) {
            break;
        }
        if attempt > 0 {
            thread::sleep(interval);
        }
        executor.reconcile(live)?;
    }
    Ok(())
}

fn independent_open_order_count<T: HyperliquidTransport>(
    transport: &mut T,
    account_address: &str,
) -> Result<usize> {
    let response = transport
        .info(&json!({"type": "openOrders", "user": account_address}))
        .map_err(|_| anyhow::anyhow!("independent open-orders query failed"))?;
    let orders = response
        .as_array()
        .context("independent open-orders response must be an array")?;
    ensure!(
        orders.iter().all(Value::is_object),
        "independent open-orders response contains a non-object"
    );
    Ok(orders.len())
}

fn is_terminal(state: OrderState) -> bool {
    matches!(
        state,
        OrderState::Filled | OrderState::Cancelled | OrderState::Rejected
    )
}

#[cfg(test)]
mod live_preflight_tests {
    use super::*;

    #[test]
    fn unified_equity_uses_available_usdc_not_legacy_perps_state() {
        let state = json!({
            "balances": [{"coin": "USDC", "token": 0, "total": "999.0"}],
            "tokenToAvailableAfterMaintenance": [[0, "998.5"], [1, "2"]]
        });
        assert_eq!(
            parse_available_equity(AccountMode::UnifiedAccount, &state).unwrap(),
            Decimal::from_str_exact("998.5").unwrap()
        );
        assert!(parse_available_equity(AccountMode::Standard, &state).is_err());
    }

    #[test]
    fn standard_equity_and_agent_master_are_strictly_validated() {
        let state = json!({"marginSummary": {"accountValue": "25.0"}});
        assert_eq!(
            parse_available_equity(AccountMode::Standard, &state).unwrap(),
            Decimal::from(25)
        );
        let role = json!({
            "role": "agent",
            "data": {"user": "0x1111111111111111111111111111111111111111"}
        });
        validate_agent_role(&role, "0x1111111111111111111111111111111111111111").unwrap();
        assert!(validate_agent_role(&role, "0x2222222222222222222222222222222222222222").is_err());
        assert!(validate_agent_role(&json!({"role": "user"}), "0x11").is_err());
    }
}

fn now_ms() -> Result<i64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes the Unix epoch")?
        .as_millis();
    i64::try_from(millis).context("system clock exceeds signed milliseconds")
}

#[derive(Clone)]
struct ConfiguredMarket {
    metadata: MarketMetadata,
    asset_id: u32,
}

impl MarketMetadataResolver for ConfiguredMarket {
    fn resolve(&self, venue: &str, market: &str) -> Result<MarketMetadata, ExecutionError> {
        if venue != self.metadata.venue || market != self.metadata.market {
            return Err(ExecutionError::Metadata(
                "requested market does not match pinned operator metadata".to_owned(),
            ));
        }
        Ok(self.metadata.clone())
    }
}

impl HyperliquidAssetResolver for ConfiguredMarket {
    fn asset_id(&self, order: &ValidatedOrder) -> Result<u32, ExecutionError> {
        if order.venue != self.metadata.venue || order.market != self.metadata.market {
            return Err(ExecutionError::Metadata(
                "validated order does not match pinned Hyperliquid asset".to_owned(),
            ));
        }
        Ok(self.asset_id)
    }
}

#[derive(Clone)]
struct ConfiguredSnapshot(RiskSnapshot);

impl RiskSnapshotSource for ConfiguredSnapshot {
    fn snapshot(&self, _order: &ValidatedOrder) -> Result<RiskSnapshot, ExecutionError> {
        Ok(self.0.clone())
    }
}
