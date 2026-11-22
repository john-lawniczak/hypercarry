//! Deterministic, non-signing isolated bracket preview. Never authorizes an order.
#![allow(missing_docs)]
use crate::account::{AccountError, MarginTable, Result, checked, fail, validate_address};
use crate::info::Network;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Long,
    Short,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanInput {
    pub network: Network,
    pub account: String,
    pub coin: String,
    pub direction: Direction,
    #[serde(with = "rust_decimal::serde::str")]
    pub notional_budget: Decimal,
    pub leverage: u32,
    #[serde(with = "rust_decimal::serde::str")]
    pub reserve_usdc: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub entry_slippage_percent: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub exit_slippage_percent: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub take_profit: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub stop_loss: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub min_health_at_stop: Decimal,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanMarket {
    pub asset: u32,
    pub size_decimals: u32,
    pub max_leverage: u32,
    #[serde(with = "rust_decimal::serde::str")]
    pub mark: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub available_usdc: Decimal,
    /// Explicit fee assumption; must be verified for this account before execution.
    #[serde(with = "rust_decimal::serde::str")]
    pub taker_fee_rate: Decimal,
    pub observed_at_ms: u64,
    pub margin_table: MarginTable,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradePlan {
    pub schema_version: u32,
    pub input: PlanInput,
    pub market: PlanMarket,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub entry_limit: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub tp_limit: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sl_limit: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub expected_notional: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub collateral_budget: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub entry_fee_budget: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub gross_pnl_at_tp: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub gross_pnl_at_sl: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub health_at_sl: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub health_at_sl_limit: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub gross_pnl_at_sl_limit: Decimal,
    pub expires_at_ms: u64,
    pub executable: bool,
    pub limitations: Vec<String>,
}

/// Valid perps price with rounding toward the interior of the allowed collar.
/// # Errors
/// Rejects nonpositive or unrepresentable prices and invalid size precision.
pub fn price_round(price: Decimal, size_decimals: u32, up: bool) -> Result<Decimal> {
    if price <= Decimal::ZERO || size_decimals > 6 {
        return Err(fail("invalid price/precision"));
    }
    let mut magnitude = price;
    let mut integer_digits = 1_i32;
    while magnitude >= Decimal::TEN {
        magnitude = checked(magnitude.checked_div(Decimal::TEN))?;
        integer_digits += 1;
    }
    while magnitude < Decimal::ONE {
        magnitude = checked(magnitude.checked_mul(Decimal::TEN))?;
        integer_digits -= 1;
    }
    let significant_dp =
        u32::try_from((5 - integer_digits).max(0)).map_err(|_| fail("precision overflow"))?;
    let dp = (6 - size_decimals).min(significant_dp);
    let value = price.round_dp_with_strategy(
        dp,
        if up {
            RoundingStrategy::ToPositiveInfinity
        } else {
            RoundingStrategy::ToNegativeInfinity
        },
    );
    if value <= Decimal::ZERO {
        return Err(fail("price rounds to zero"));
    }
    Ok(value)
}
fn pct(value: Decimal) -> Result<Decimal> {
    checked(value.checked_div(Decimal::ONE_HUNDRED))
}
fn pnl(direction: Direction, q: Decimal, entry: Decimal, exit: Decimal) -> Result<Decimal> {
    let change = if direction == Direction::Short {
        checked(entry.checked_sub(exit))?
    } else {
        checked(exit.checked_sub(entry))?
    };
    checked(q.checked_mul(change))
}
/// Preview includes stale-data, precision, free collateral and stop-health checks.
/// All arithmetic is checked; returned plans are deliberately not submission capabilities.
/// # Errors
/// Rejects stale data, invalid bounds, insufficient collateral or stop-health cushion.
// Keep dependent checked arithmetic in one auditable calculation.
#[allow(clippy::too_many_lines)]
pub fn preview(input: PlanInput, market: PlanMarket, now: u64) -> Result<TradePlan> {
    validate_address(&input.account)?;
    if input.coin.len() > 32
        || input.coin.split(':').count() > 2
        || !input
            .coin
            .split(':')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return Err(fail("invalid scoped market name"));
    }
    if market.observed_at_ms > now || now - market.observed_at_ms > 10_000 {
        return Err(fail("market/account snapshot is stale or from the future"));
    }
    if market.mark <= Decimal::ZERO
        || market.size_decimals > 6
        || market.available_usdc < Decimal::ZERO
        || input.notional_budget < Decimal::TEN
        || input.reserve_usdc < Decimal::ZERO
        || input.leverage == 0
        || input.leverage > market.max_leverage
        || market.taker_fee_rate < Decimal::ZERO
        || market.taker_fee_rate > Decimal::new(1, 2)
        || input.entry_slippage_percent <= Decimal::ZERO
        || input.entry_slippage_percent > Decimal::from(5)
        || input.exit_slippage_percent <= Decimal::ZERO
        || input.exit_slippage_percent > Decimal::TEN
        || input.min_health_at_stop <= Decimal::ONE
    {
        return Err(fail(
            "invalid budget, fee, leverage, reserve, health target, or slippage bounds",
        ));
    }
    market.margin_table.validate()?;
    let long = input.direction == Direction::Long;
    let s = pct(input.entry_slippage_percent)?;
    let exit_s = pct(input.exit_slippage_percent)?;
    let ceiling = checked(
        market
            .mark
            .checked_mul(checked(Decimal::ONE.checked_add(s))?),
    )?;
    let floor = checked(
        market
            .mark
            .checked_mul(checked(Decimal::ONE.checked_sub(s))?),
    )?;
    let entry_limit = price_round(
        if long { ceiling } else { floor },
        market.size_decimals,
        !long,
    )?;
    let q = checked(input.notional_budget.checked_div(ceiling))?
        .round_dp_with_strategy(market.size_decimals, RoundingStrategy::ToZero);
    let expected_notional = checked(q.checked_mul(market.mark))?;
    if q <= Decimal::ZERO || checked(q.checked_mul(floor))? < Decimal::TEN {
        return Err(fail("rounded quantity is below venue minimum notional"));
    }
    let valid_triggers = if long {
        input.take_profit > ceiling && input.stop_loss < floor && input.stop_loss > Decimal::ZERO
    } else {
        input.take_profit > Decimal::ZERO && input.take_profit < floor && input.stop_loss > ceiling
    };
    if !valid_triggers {
        return Err(fail(
            "TP/SL must be on the correct sides of the entire entry collar",
        ));
    }
    for price in [input.take_profit, input.stop_loss] {
        if price_round(price, market.size_decimals, false)? != price {
            return Err(fail("trigger price violates venue precision"));
        }
    }
    let exit_factor = if long {
        checked(Decimal::ONE.checked_sub(exit_s))?
    } else {
        checked(Decimal::ONE.checked_add(exit_s))?
    };
    let tp_limit = price_round(
        checked(input.take_profit.checked_mul(exit_factor))?,
        market.size_decimals,
        long,
    )?;
    let sl_limit = price_round(
        checked(input.stop_loss.checked_mul(exit_factor))?,
        market.size_decimals,
        long,
    )?;
    let reserved_notional = checked(q.checked_mul(ceiling))?;
    let collateral = checked(reserved_notional.checked_div(Decimal::from(input.leverage)))?;
    let fee = checked(reserved_notional.checked_mul(market.taker_fee_rate))?;
    let required = checked(checked(collateral.checked_add(fee))?.checked_add(input.reserve_usdc))?;
    if required > market.available_usdc {
        return Err(fail(
            "insufficient available collateral after fees and reserve",
        ));
    }
    // Use minimum entry margin and worst entry price, rather than the larger reserved budget.
    let min_margin =
        checked(checked(q.checked_mul(floor))?.checked_div(Decimal::from(input.leverage)))?;
    let stop_pnl = pnl(
        input.direction,
        q,
        if long { ceiling } else { floor },
        input.stop_loss,
    )?;
    let equity = checked(checked(min_margin.checked_sub(fee))?.checked_add(stop_pnl))?;
    let health = checked(
        equity.checked_div(
            market
                .margin_table
                .maintenance(checked(q.checked_mul(input.stop_loss))?)?,
        ),
    )?;
    if health < input.min_health_at_stop {
        return Err(fail(
            "insufficient maintenance margin cushion at stop trigger",
        ));
    }
    let limit_equity = checked(checked(min_margin.checked_sub(fee))?.checked_add(pnl(
        input.direction,
        q,
        if long { ceiling } else { floor },
        sl_limit,
    )?))?;
    let limit_health = checked(
        limit_equity.checked_div(
            market
                .margin_table
                .maintenance(checked(q.checked_mul(sl_limit))?)?,
        ),
    )?;
    if limit_health <= Decimal::ONE {
        return Err(fail(
            "stop execution bound reaches liquidation under preview assumptions",
        ));
    }
    let tp = pnl(input.direction, q, market.mark, input.take_profit)?;
    let sl = pnl(input.direction, q, market.mark, input.stop_loss)?;
    let sl_limit_pnl = pnl(input.direction, q, market.mark, sl_limit)?;
    Ok(TradePlan {
        schema_version: 1,
        input,
        market,
        quantity: q,
        entry_limit,
        tp_limit,
        sl_limit,
        expected_notional,
        collateral_budget: collateral,
        entry_fee_budget: fee,
        gross_pnl_at_tp: tp,
        gross_pnl_at_sl: sl,
        health_at_sl: health,
        health_at_sl_limit:limit_health,
        gross_pnl_at_sl_limit:sl_limit_pnl,
        expires_at_ms: now
            .checked_add(30_000)
            .ok_or_else(|| fail("expiry overflow"))?,
        executable: false,
        limitations: vec![
            "Preview only; no signature or release authorization".into(),
            "Budget and P&L depend on actual fills; funding and exit fees excluded".into(),
            "Short IOC entry has a minimum sell price, not a maximum fill price; favorable price gaps can exceed the estimated notional/collateral budget".into(),
            "Stop is a trigger, not a guaranteed maximum loss; execution can be partial".into(),
            "Native perps, isolated margin, Unified USDC availability only".into(),
        ],
    })
}

impl std::str::FromStr for Direction {
    type Err = AccountError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "long" => Ok(Self::Long),
            "short" => Ok(Self::Short),
            _ => Err(fail("direction must be long or short")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::MarginTier;
    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }
    fn fixture() -> (PlanInput, PlanMarket) {
        (
            PlanInput {
                network: Network::Testnet,
                account: format!("0x{}", "12".repeat(20)),
                coin: "EXAMPLE".into(),
                direction: Direction::Short,
                notional_budget: d("90"),
                leverage: 1,
                reserve_usdc: d("10"),
                entry_slippage_percent: d("0.5"),
                exit_slippage_percent: d("10"),
                take_profit: d("70"),
                stop_loss: d("95"),
                min_health_at_stop: d("2"),
            },
            PlanMarket {
                asset: 2,
                size_decimals: 2,
                max_leverage: 20,
                mark: d("82"),
                available_usdc: d("105"),
                taker_fee_rate: d("0.0005"),
                observed_at_ms: 1000,
                margin_table: MarginTable {
                    margin_tiers: vec![MarginTier {
                        lower_bound: Decimal::ZERO,
                        max_leverage: 20,
                    }],
                },
            },
        )
    }
    #[test]
    fn bounded_short_preview_and_percent_units() {
        let (i, m) = fixture();
        let p = preview(i.clone(), m.clone(), 1000).unwrap();
        assert_eq!(p.entry_limit, d("81.59"));
        assert_eq!(p.sl_limit, d("104.5"));
        assert_eq!(p.quantity, d("1.09"));
        assert!(!p.executable);
        let mut five = i;
        five.entry_slippage_percent = d("5");
        let p5 = preview(five, m, 1000).unwrap();
        assert_eq!(p5.entry_limit, d("77.9"));
        assert!(p5.quantity < p.quantity);
    }
    #[test]
    fn insufficient_funds_and_reversed_triggers_fail() {
        let (mut i, mut m) = fixture();
        m.available_usdc = d("15");
        assert!(preview(i.clone(), m.clone(), 1000).is_err());
        m.available_usdc = d("105");
        i.stop_loss = d("75");
        assert!(preview(i, m, 1000).is_err());
    }
    #[test]
    fn long_and_stale_snapshots() {
        let (mut i, m) = fixture();
        i.direction = Direction::Long;
        i.take_profit = d("95");
        i.stop_loss = d("70");
        let p = preview(i.clone(), m.clone(), 1000).unwrap();
        assert_eq!(p.entry_limit, d("82.41"));
        assert_eq!(p.sl_limit, d("63"));
        assert!(preview(i, m, 11001).is_err());
    }
    #[test]
    fn precision_collars_round_inward() {
        assert_eq!(
            price_round(d("0.001234567"), 0, false).unwrap(),
            d("0.001234")
        );
        assert_eq!(price_round(d("12345.678"), 2, true).unwrap(), d("12346"));
        assert!(price_round(d("0.00000000001"), 6, false).is_err());
    }
    #[test]
    fn builder_market_names_preserve_scope_and_reject_paths() {
        let (mut input, market) = fixture();
        input.coin = "xyz:XYZ100".into();
        assert_eq!(
            preview(input.clone(), market.clone(), 1000)
                .unwrap()
                .input
                .coin,
            "xyz:XYZ100"
        );
        for bad in [":BTC", "xyz:", "a:b:c", "../BTC", ""] {
            input.coin = bad.into();
            assert!(preview(input.clone(), market.clone(), 1000).is_err());
        }
    }
}
