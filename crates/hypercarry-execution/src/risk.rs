use crate::{ExecutionError, RiskDecision, RiskPolicy, ValidatedOrder};
use rust_decimal::Decimal;
use std::{collections::BTreeSet, fs, io::ErrorKind, path::PathBuf};

const BPS_DENOMINATOR: u32 = 10_000;

/// Complete, exact risk limits for one execution process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskLimits {
    /// Allowlisted `venue:market` entries eligible for execution.
    pub allowed_markets: BTreeSet<String>,
    /// Maximum notional for a single order.
    pub max_order_notional: Decimal,
    /// Maximum aggregate open notional across orders.
    pub max_aggregate_notional: Decimal,
    /// Maximum limit-price deviation from reference, in basis points.
    pub max_price_deviation_bps: Decimal,
    /// Maximum permitted leverage.
    pub max_leverage: Decimal,
    /// Maximum orders allowed within the frequency window.
    pub max_orders_per_window: usize,
    /// Length of the order-frequency window, milliseconds.
    pub order_frequency_window_ms: i64,
    /// Maximum concurrently open orders.
    pub max_open_orders: usize,
    /// Maximum tolerated trailing realized loss.
    pub max_rolling_loss: Decimal,
    /// Maximum tolerated age of reference market data, milliseconds.
    pub max_market_data_age_ms: i64,
}

impl RiskLimits {
    /// Validates every configured boundary.
    ///
    /// Market allowlist entries use the exact `venue:market` representation.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty allowlist or a non-positive/invalid limit.
    pub fn validate(&self) -> Result<(), ExecutionError> {
        if self.allowed_markets.is_empty()
            || self
                .allowed_markets
                .iter()
                .any(|market| market.trim().is_empty() || !market.contains(':'))
        {
            return Err(policy_error(
                "allowed_markets must contain valid exact `venue:market` entries",
            ));
        }
        for (name, value) in [
            ("max_order_notional", self.max_order_notional),
            ("max_aggregate_notional", self.max_aggregate_notional),
            ("max_leverage", self.max_leverage),
        ] {
            if value <= Decimal::ZERO {
                return Err(policy_error(format!("{name} must be positive")));
            }
        }
        if self.max_price_deviation_bps < Decimal::ZERO
            || self.max_price_deviation_bps >= Decimal::from(BPS_DENOMINATOR)
        {
            return Err(policy_error(
                "max_price_deviation_bps must be in [0, 10000)",
            ));
        }
        if self.max_rolling_loss < Decimal::ZERO {
            return Err(policy_error("max_rolling_loss must not be negative"));
        }
        if self.max_orders_per_window == 0
            || self.order_frequency_window_ms <= 0
            || self.max_open_orders == 0
            || self.max_market_data_age_ms < 0
        {
            return Err(policy_error(
                "frequency/open-order counts must be positive and market-data age must not be negative",
            ));
        }
        Ok(())
    }
}

/// Point-in-time account and market inputs used by the deterministic policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskSnapshot {
    /// Time the snapshot was taken, unix milliseconds.
    pub observed_at_ms: i64,
    /// Time of the reference market data, unix milliseconds.
    pub market_data_at_ms: i64,
    /// Reference price used for deviation and notional checks.
    pub reference_price: Decimal,
    /// Aggregate open notional across current orders.
    pub aggregate_notional: Decimal,
    /// Current account equity.
    pub account_equity: Decimal,
    /// Number of currently open orders.
    pub open_order_count: usize,
    /// Recent order submission times for frequency limiting.
    pub recent_order_times_ms: Vec<i64>,
    /// Signed realized `PnL` in the configured rolling window; loss is negative.
    pub rolling_pnl: Decimal,
}

/// Fail-closed source for the exact market/account snapshot used by policy.
pub trait RiskSnapshotSource {
    /// Returns a snapshot for this order, or an error if one cannot be trusted.
    ///
    /// # Errors
    ///
    /// Returns an error for unavailable, inconsistent, or incomplete state.
    fn snapshot(&self, order: &ValidatedOrder) -> Result<RiskSnapshot, ExecutionError>;
}

/// Process-independent emergency-stop observation.
pub trait KillSwitch {
    /// Reports whether execution is disabled.
    ///
    /// # Errors
    ///
    /// Returns an error when switch state cannot be established. Policies must
    /// treat such errors as engaged/fail-closed.
    fn is_engaged(&self) -> Result<bool, ExecutionError>;
}

/// Filesystem kill switch: the configured path's existence disables execution.
#[derive(Debug, Clone)]
pub struct FileKillSwitch {
    path: PathBuf,
}

impl FileKillSwitch {
    /// Watch `path`; its existence engages the switch.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Path whose existence disables execution.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl KillSwitch for FileKillSwitch {
    fn is_engaged(&self) -> Result<bool, ExecutionError> {
        match fs::metadata(&self.path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(policy_error(format!(
                "cannot read kill switch {}: {error}",
                self.path.display()
            ))),
        }
    }
}

/// Comprehensive M7 risk policy with a stable, ordered decision path.
pub struct ComprehensiveRiskPolicy<S, K> {
    limits: RiskLimits,
    snapshots: S,
    kill_switch: K,
}

impl<S, K> ComprehensiveRiskPolicy<S, K> {
    /// Creates a policy after validating all configured limits.
    ///
    /// # Errors
    ///
    /// Returns an error when any limit is invalid.
    pub fn new(limits: RiskLimits, snapshots: S, kill_switch: K) -> Result<Self, ExecutionError> {
        limits.validate()?;
        Ok(Self {
            limits,
            snapshots,
            kill_switch,
        })
    }

    /// Borrow the configured risk limits.
    pub fn limits(&self) -> &RiskLimits {
        &self.limits
    }
}

impl<S, K> RiskPolicy for ComprehensiveRiskPolicy<S, K>
where
    S: RiskSnapshotSource,
    K: KillSwitch,
{
    // Keep the safety checks in one explicit order so the first rejection is
    // deterministic and the complete policy is auditable as a single path.
    #[allow(clippy::too_many_lines)]
    fn evaluate(&self, order: &ValidatedOrder) -> Result<RiskDecision, ExecutionError> {
        order.validate()?;
        self.limits.validate()?;
        if self.kill_switch.is_engaged()? {
            return Ok(reject(
                "kill_switch",
                "process-independent kill switch is engaged",
            ));
        }

        let market_key = format!("{}:{}", order.venue, order.market);
        if !self.limits.allowed_markets.contains(&market_key) {
            return Ok(reject(
                "market_not_allowed",
                format!("market {market_key} is not in the exact allowlist"),
            ));
        }

        let snapshot = self.snapshots.snapshot(order)?;
        validate_snapshot(&snapshot)?;
        let market_age_ms = snapshot
            .observed_at_ms
            .checked_sub(snapshot.market_data_at_ms)
            .ok_or_else(|| policy_error("market-data age overflow"))?;
        if market_age_ms < 0 {
            return Err(policy_error("market data timestamp is in the future"));
        }
        if market_age_ms > self.limits.max_market_data_age_ms {
            return Ok(reject(
                "stale_market_data",
                format!(
                    "market data age {market_age_ms}ms exceeds {}ms",
                    self.limits.max_market_data_age_ms
                ),
            ));
        }

        let notional = order
            .quantity
            .checked_mul(order.limit_price)
            .ok_or_else(|| policy_error("order notional overflow"))?
            .normalize();
        if notional > self.limits.max_order_notional {
            return Ok(reject(
                "max_order_notional",
                format!(
                    "order notional {notional} exceeds {}",
                    self.limits.max_order_notional
                ),
            ));
        }
        let projected_notional = snapshot
            .aggregate_notional
            .checked_add(notional)
            .ok_or_else(|| policy_error("aggregate notional overflow"))?;
        if projected_notional > self.limits.max_aggregate_notional {
            return Ok(reject(
                "max_aggregate_notional",
                format!(
                    "projected aggregate notional {projected_notional} exceeds {}",
                    self.limits.max_aggregate_notional
                ),
            ));
        }

        let deviation_bps = order
            .limit_price
            .checked_sub(snapshot.reference_price)
            .and_then(checked_abs)
            .and_then(|deviation| deviation.checked_div(snapshot.reference_price))
            .and_then(|ratio| ratio.checked_mul(Decimal::from(BPS_DENOMINATOR)))
            .ok_or_else(|| policy_error("price deviation overflow"))?
            .normalize();
        if deviation_bps > self.limits.max_price_deviation_bps {
            return Ok(reject(
                "max_price_deviation",
                format!(
                    "limit deviation {deviation_bps}bps exceeds {}bps",
                    self.limits.max_price_deviation_bps
                ),
            ));
        }

        let projected_leverage = projected_notional
            .checked_div(snapshot.account_equity)
            .ok_or_else(|| policy_error("projected leverage overflow"))?
            .normalize();
        if projected_leverage > self.limits.max_leverage {
            return Ok(reject(
                "max_leverage",
                format!(
                    "projected leverage {projected_leverage}x exceeds {}x",
                    self.limits.max_leverage
                ),
            ));
        }
        if snapshot.open_order_count >= self.limits.max_open_orders {
            return Ok(reject(
                "max_open_orders",
                format!(
                    "open order count {} reached limit {}",
                    snapshot.open_order_count, self.limits.max_open_orders
                ),
            ));
        }

        let window_start = snapshot
            .observed_at_ms
            .checked_sub(self.limits.order_frequency_window_ms)
            .ok_or_else(|| policy_error("order-frequency window overflow"))?;
        if snapshot
            .recent_order_times_ms
            .iter()
            .any(|time| *time > snapshot.observed_at_ms)
        {
            return Err(policy_error("recent order timestamp is in the future"));
        }
        let recent_count = snapshot
            .recent_order_times_ms
            .iter()
            .filter(|time| **time >= window_start)
            .count();
        if recent_count >= self.limits.max_orders_per_window {
            return Ok(reject(
                "order_frequency",
                format!(
                    "{recent_count} orders already occurred in the inclusive {}ms window",
                    self.limits.order_frequency_window_ms
                ),
            ));
        }

        let rolling_loss = if snapshot.rolling_pnl < Decimal::ZERO {
            Decimal::ZERO
                .checked_sub(snapshot.rolling_pnl)
                .ok_or_else(|| policy_error("rolling loss overflow"))?
        } else {
            Decimal::ZERO
        };
        if rolling_loss > self.limits.max_rolling_loss {
            return Ok(reject(
                "max_rolling_loss",
                format!(
                    "rolling loss {rolling_loss} exceeds {}",
                    self.limits.max_rolling_loss
                ),
            ));
        }

        Ok(RiskDecision::Allow {
            code: "all_limits_satisfied".to_owned(),
            reason: format!(
                "allowed {market_key} notional {notional} at {deviation_bps}bps deviation"
            ),
        })
    }

    fn kill_switch_engaged(&self) -> Result<bool, ExecutionError> {
        self.kill_switch.is_engaged()
    }
}

fn validate_snapshot(snapshot: &RiskSnapshot) -> Result<(), ExecutionError> {
    if snapshot.observed_at_ms < 0 || snapshot.market_data_at_ms < 0 {
        return Err(policy_error(
            "risk snapshot timestamps must not be negative",
        ));
    }
    if snapshot.reference_price <= Decimal::ZERO
        || snapshot.aggregate_notional < Decimal::ZERO
        || snapshot.account_equity <= Decimal::ZERO
    {
        return Err(policy_error(
            "snapshot requires positive reference/equity and non-negative aggregate notional",
        ));
    }
    Ok(())
}

fn checked_abs(value: Decimal) -> Option<Decimal> {
    if value < Decimal::ZERO {
        Decimal::ZERO.checked_sub(value)
    } else {
        Some(value)
    }
}

fn reject(code: impl Into<String>, reason: impl Into<String>) -> RiskDecision {
    RiskDecision::Reject {
        code: code.into(),
        reason: reason.into(),
    }
}

fn policy_error(message: impl Into<String>) -> ExecutionError {
    ExecutionError::Policy(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MarketMetadata, OrderIntent, Side, ValidatedOrder};

    #[derive(Clone)]
    struct Snapshot(RiskSnapshot);

    impl RiskSnapshotSource for Snapshot {
        fn snapshot(&self, _order: &ValidatedOrder) -> Result<RiskSnapshot, ExecutionError> {
            Ok(self.0.clone())
        }
    }

    struct Switch(bool);

    impl KillSwitch for Switch {
        fn is_engaged(&self) -> Result<bool, ExecutionError> {
            Ok(self.0)
        }
    }

    #[test]
    fn exact_boundaries_are_allowed_and_explained() {
        let mut snapshot = baseline_snapshot();
        snapshot.aggregate_notional = dec("900");
        snapshot.account_equity = dec("200");
        snapshot.open_order_count = 1;
        snapshot.recent_order_times_ms = vec![9_000];
        snapshot.rolling_pnl = dec("-50");
        let policy =
            ComprehensiveRiskPolicy::new(limits(), Snapshot(snapshot), Switch(false)).unwrap();

        assert!(matches!(
            policy.evaluate(&order()).unwrap(),
            RiskDecision::Allow { code, .. } if code == "all_limits_satisfied"
        ));
    }

    #[test]
    fn each_limit_rejects_immediately_beyond_its_boundary() {
        let cases = [
            ("stale_market_data", mutate(|s| s.market_data_at_ms = 8_999)),
            ("max_order_notional", mutate_order("1.01", "100")),
            (
                "max_aggregate_notional",
                mutate(|s| s.aggregate_notional = dec("901")),
            ),
            ("max_price_deviation", mutate_order("0.99", "101.01")),
            (
                "max_leverage",
                mutate(|s| {
                    s.aggregate_notional = dec("900");
                    s.account_equity = dec("199");
                }),
            ),
            ("max_open_orders", mutate(|s| s.open_order_count = 2)),
            (
                "order_frequency",
                mutate(|s| s.recent_order_times_ms = vec![9_000, 10_000]),
            ),
            (
                "max_rolling_loss",
                mutate(|s| s.rolling_pnl = dec("-50.01")),
            ),
        ];
        for (expected, (snapshot, order)) in cases {
            let policy =
                ComprehensiveRiskPolicy::new(limits(), Snapshot(snapshot), Switch(false)).unwrap();
            assert!(matches!(
                policy.evaluate(&order).unwrap(),
                RiskDecision::Reject { code, .. } if code == expected
            ));
        }
    }

    #[test]
    fn kill_switch_and_untrusted_snapshot_fail_closed() {
        let engaged =
            ComprehensiveRiskPolicy::new(limits(), Snapshot(baseline_snapshot()), Switch(true))
                .unwrap();
        assert!(matches!(
            engaged.evaluate(&order()).unwrap(),
            RiskDecision::Reject { code, .. } if code == "kill_switch"
        ));

        let mut future = baseline_snapshot();
        future.market_data_at_ms = future.observed_at_ms + 1;
        let invalid =
            ComprehensiveRiskPolicy::new(limits(), Snapshot(future), Switch(false)).unwrap();
        assert!(invalid.evaluate(&order()).is_err());
    }

    #[test]
    fn decimal_overflow_fails_closed_without_panicking() {
        let policy =
            ComprehensiveRiskPolicy::new(limits(), Snapshot(baseline_snapshot()), Switch(false))
                .unwrap();
        let overflow = make_order(&Decimal::MAX.to_string(), "2");
        assert!(policy.evaluate(&overflow).is_err());

        let mut snapshot = baseline_snapshot();
        snapshot.rolling_pnl = Decimal::MIN;
        let policy =
            ComprehensiveRiskPolicy::new(limits(), Snapshot(snapshot), Switch(false)).unwrap();
        assert!(matches!(
            policy.evaluate(&order()).unwrap(),
            RiskDecision::Reject { code, .. } if code == "max_rolling_loss"
        ));
    }

    #[test]
    fn filesystem_switch_is_process_independent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("STOP");
        let switch = FileKillSwitch::new(&path);
        assert!(!switch.is_engaged().unwrap());
        std::fs::write(&path, b"operator stop\n").unwrap();
        assert!(switch.is_engaged().unwrap());
    }

    type Case = (RiskSnapshot, ValidatedOrder);

    fn mutate(change: impl FnOnce(&mut RiskSnapshot)) -> Case {
        let mut snapshot = baseline_snapshot();
        change(&mut snapshot);
        (snapshot, order())
    }

    fn mutate_order(quantity: &str, price: &str) -> Case {
        (baseline_snapshot(), make_order(quantity, price))
    }

    fn limits() -> RiskLimits {
        RiskLimits {
            allowed_markets: BTreeSet::from(["hyperliquid:BTC".to_owned()]),
            max_order_notional: dec("100"),
            max_aggregate_notional: dec("1000"),
            max_price_deviation_bps: dec("100"),
            max_leverage: dec("5"),
            max_orders_per_window: 2,
            order_frequency_window_ms: 1_000,
            max_open_orders: 2,
            max_rolling_loss: dec("50"),
            max_market_data_age_ms: 1_000,
        }
    }

    fn baseline_snapshot() -> RiskSnapshot {
        RiskSnapshot {
            observed_at_ms: 10_000,
            market_data_at_ms: 9_000,
            reference_price: dec("100"),
            aggregate_notional: Decimal::ZERO,
            account_equity: dec("1000"),
            open_order_count: 0,
            recent_order_times_ms: Vec::new(),
            rolling_pnl: Decimal::ZERO,
        }
    }

    fn order() -> ValidatedOrder {
        make_order("1", "100")
    }

    fn make_order(quantity: &str, price: &str) -> ValidatedOrder {
        let metadata = MarketMetadata::new(
            "hyperliquid",
            "BTC",
            dec("0.01"),
            dec("0.001"),
            dec("0.001"),
        )
        .unwrap();
        let intent = OrderIntent::limit(
            "risk-test",
            "hyperliquid",
            "BTC",
            Side::Buy,
            dec(quantity),
            dec(price),
            10_000,
        )
        .unwrap();
        ValidatedOrder::resolve(&intent, &metadata).unwrap()
    }

    fn dec(value: &str) -> Decimal {
        value.parse().unwrap()
    }
}
