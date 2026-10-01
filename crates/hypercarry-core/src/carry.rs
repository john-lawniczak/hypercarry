//! Carry trade profit-and-loss ledger.
//!
//! [`metrics`](crate::metrics) answers a market question: what is funding worth
//! per year at this rate. This module answers the position question that sits
//! between a funding dataset and a trading system: what did one specific carry
//! trade actually earn, after funding accrual, price movement on both legs, and
//! fees.
//!
//! The calculation is pure, offline, and exact. Every fund-affecting operation
//! is checked, so an out-of-range input returns [`CarryError::Overflow`] instead
//! of panicking or silently wrapping.
//!
//! # Funding sign convention
//!
//! A positive funding rate means longs pay shorts. A short perpetual position
//! therefore *receives* positive funding and a long position *pays* it. The
//! signs invert for a negative rate. [`CarrySide::funding_sign`] is the single
//! place this convention is encoded.
//!
//! # Notional valuation is the caller's decision
//!
//! Funding is charged on position notional at each settlement, so valuing a
//! settlement requires a price. This module takes that price per settlement in
//! [`FundingSettlement::mark_price`] and never invents one. A caller replaying a
//! dataset that stores no per-settlement mark may pass the entry price to get
//! constant-notional accounting, but that is an approximation the caller has
//! chosen and should report as such — it is not a property of this ledger.

use rust_decimal::Decimal;
use std::{error::Error, fmt};

/// Hours in a 365-day year, used to annualize a realized result.
const HOURS_PER_YEAR: u32 = 365 * 24;

/// Side held on the perpetual leg of a carry trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarrySide {
    /// Long the perpetual. Pays funding when the rate is positive.
    Long,
    /// Short the perpetual. Receives funding when the rate is positive.
    Short,
}

impl CarrySide {
    /// Sign applied to a funding payment for this side.
    ///
    /// Positive means the position receives funding when the rate is positive.
    #[must_use]
    pub fn funding_sign(self) -> Decimal {
        match self {
            Self::Long => -Decimal::ONE,
            Self::Short => Decimal::ONE,
        }
    }

    /// Sign applied to price movement on the perpetual leg.
    ///
    /// This is always the inverse of [`Self::funding_sign`]: the side that
    /// receives funding is the side that loses when the perpetual rallies.
    #[must_use]
    pub fn price_sign(self) -> Decimal {
        match self {
            Self::Long => Decimal::ONE,
            Self::Short => -Decimal::ONE,
        }
    }
}

impl fmt::Display for CarrySide {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Long => "long",
            Self::Short => "short",
        })
    }
}

/// One settled funding observation applied to a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FundingSettlement {
    /// Funding settlement time in Unix milliseconds.
    pub settlement_time_ms: i64,
    /// Settled funding rate for the whole settlement interval, not per hour.
    pub funding_rate: Decimal,
    /// Price used to value the position at this settlement.
    ///
    /// Must be strictly positive. A zero or negative valuation price would
    /// erase or invert the sign of the funding cash flow, which is a money bug
    /// rather than a representable market state, so it is rejected at the
    /// boundary.
    pub mark_price: Decimal,
}

/// Proportional fee rates charged on traded notional, as a fraction of it.
///
/// A `0.00045` rate is 4.5 basis points. Rates apply per leg and per side of the
/// round trip: a hedged trade that opens and closes pays four charges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeSchedule {
    /// Fee rate charged when a leg is opened.
    pub entry_rate: Decimal,
    /// Fee rate charged when a leg is closed.
    pub exit_rate: Decimal,
}

impl FeeSchedule {
    /// A schedule that charges nothing, for modelling gross results.
    #[must_use]
    pub fn zero() -> Self {
        Self {
            entry_rate: Decimal::ZERO,
            exit_rate: Decimal::ZERO,
        }
    }

    /// A schedule charging the same rate on entry and exit.
    #[must_use]
    pub fn flat(rate: Decimal) -> Self {
        Self {
            entry_rate: rate,
            exit_rate: rate,
        }
    }
}

/// Closing prices for a carry trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarryExit {
    /// Perpetual price at which the leg was closed.
    pub perp_price: Decimal,
    /// Spot price at which the hedge was closed.
    ///
    /// Must be present when, and only when, the trade was opened with a hedge.
    pub spot_price: Option<Decimal>,
}

/// A carry trade: a perpetual leg and an optional opposing spot hedge.
///
/// The hedge is assumed delta neutral — the same base-asset size, held in the
/// opposite direction — which is what makes the result a funding capture rather
/// than a directional bet. An unhedged trade is accepted and reported honestly:
/// its perpetual price result flows straight into the net.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarryTrade {
    /// Side held on the perpetual leg.
    pub side: CarrySide,
    /// Base-asset size on each leg. Must be strictly positive.
    pub size: Decimal,
    /// Perpetual price at which the leg was opened. Must be strictly positive.
    pub perp_entry_price: Decimal,
    /// Spot price at which the hedge was opened.
    ///
    /// `None` is an unhedged, directional carry.
    pub spot_entry_price: Option<Decimal>,
    /// Closing prices, or `None` while the trade is still open.
    pub exit: Option<CarryExit>,
    /// Fee rates applied to each leg traded.
    pub fees: FeeSchedule,
}

/// Decomposed result of one carry trade.
///
/// The components sum exactly to [`Self::net`]; no rounding is applied at any
/// step, so a caller may present the decomposition without it disagreeing with
/// the total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CarryPnl {
    /// Funding accrued across every applied settlement. Positive is received.
    pub funding: Decimal,
    /// Price result on the perpetual leg. Zero while the trade is open.
    pub perp_price_pnl: Decimal,
    /// Price result on the spot hedge. Zero when unhedged or still open.
    pub spot_price_pnl: Decimal,
    /// Total fees charged. Always zero or positive, and subtracted from net.
    pub fees: Decimal,
    /// `funding + perp_price_pnl + spot_price_pnl - fees`.
    pub net: Decimal,
    /// Perpetual notional at entry: `size * perp_entry_price`.
    pub entry_notional: Decimal,
    /// Number of settlements applied.
    pub settlements: usize,
    /// Earliest applied settlement time, or `None` when none were applied.
    pub first_settlement_ms: Option<i64>,
    /// Latest applied settlement time, or `None` when none were applied.
    pub last_settlement_ms: Option<i64>,
    /// Whether the trade was closed. An open trade reports funding only.
    pub closed: bool,
}

/// Error returned when a carry trade cannot be evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarryError {
    /// Size is zero or negative. Direction is carried by [`CarrySide`], so a
    /// signed size would express the same fact twice and could contradict it.
    NonPositiveSize {
        /// Rejected size.
        size: Decimal,
    },
    /// A price that must be strictly positive was not.
    NonPositivePrice {
        /// Which price was rejected.
        field: &'static str,
        /// Rejected value.
        price: Decimal,
    },
    /// A fee rate was negative, which would pay the trader to trade.
    NegativeFeeRate {
        /// Rejected rate.
        rate: Decimal,
    },
    /// The hedge is described inconsistently between entry and exit.
    HedgeMismatch,
    /// Settlements were not strictly increasing in time.
    ///
    /// Repeated or out-of-order settlements double-count funding, so they fail
    /// closed rather than producing a plausible wrong number.
    UnorderedSettlements {
        /// Time of the offending settlement.
        settlement_time_ms: i64,
    },
    /// An intermediate or final value exceeded the representable range.
    Overflow,
}

impl fmt::Display for CarryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonPositiveSize { size } => {
                write!(formatter, "carry size must be strictly positive: {size}")
            }
            Self::NonPositivePrice { field, price } => {
                write!(formatter, "{field} must be strictly positive: {price}")
            }
            Self::NegativeFeeRate { rate } => {
                write!(formatter, "fee rate must not be negative: {rate}")
            }
            Self::HedgeMismatch => formatter.write_str(
                "a hedged trade must close its spot leg, and an unhedged trade must not",
            ),
            Self::UnorderedSettlements { settlement_time_ms } => write!(
                formatter,
                "settlements must be strictly increasing in time; {settlement_time_ms} is not"
            ),
            Self::Overflow => {
                formatter.write_str("carry result exceeds the representable decimal range")
            }
        }
    }
}

impl Error for CarryError {}

impl CarryTrade {
    /// Evaluate the trade against a sequence of settled funding observations.
    ///
    /// `settlements` must be strictly increasing in time and should already be
    /// filtered to the window the position was actually held. This function
    /// applies every settlement it is given; it does not infer a holding period
    /// from the entry and exit prices, because only the caller knows when the
    /// position existed.
    ///
    /// # Errors
    ///
    /// Returns [`CarryError`] for a non-positive size or price, a negative fee
    /// rate, an inconsistently described hedge, non-increasing settlements, or
    /// a value outside the representable `Decimal` range.
    ///
    /// # Examples
    ///
    /// A short perpetual collects positive funding:
    ///
    /// ```
    /// use hypercarry_core::carry::{
    ///     CarrySide, CarryTrade, FeeSchedule, FundingSettlement,
    /// };
    /// use rust_decimal::Decimal;
    ///
    /// let trade = CarryTrade {
    ///     side: CarrySide::Short,
    ///     size: Decimal::ONE,
    ///     perp_entry_price: Decimal::from(80_000),
    ///     spot_entry_price: Some(Decimal::from(80_000)),
    ///     exit: None,
    ///     fees: FeeSchedule::zero(),
    /// };
    /// let settlements = [FundingSettlement {
    ///     settlement_time_ms: 1_789_761_600_036,
    ///     funding_rate: Decimal::new(125, 7), // 0.0000125
    ///     mark_price: Decimal::from(80_000),
    /// }];
    ///
    /// let pnl = trade.evaluate(&settlements)?;
    /// assert_eq!(pnl.funding, Decimal::ONE); // 80_000 * 0.0000125
    /// assert_eq!(pnl.net, Decimal::ONE);
    /// assert!(!pnl.closed);
    /// # Ok::<(), hypercarry_core::carry::CarryError>(())
    /// ```
    pub fn evaluate(&self, settlements: &[FundingSettlement]) -> Result<CarryPnl, CarryError> {
        self.validate()?;

        let funding = self.accrue_funding(settlements)?;
        let (perp_price_pnl, spot_price_pnl) = self.price_result()?;
        let fees = self.fees()?;
        let entry_notional = mul(self.size, self.perp_entry_price)?;

        let net = sub(add(add(funding, perp_price_pnl)?, spot_price_pnl)?, fees)?;

        Ok(CarryPnl {
            funding,
            perp_price_pnl,
            spot_price_pnl,
            fees,
            net,
            entry_notional,
            settlements: settlements.len(),
            first_settlement_ms: settlements.first().map(|s| s.settlement_time_ms),
            last_settlement_ms: settlements.last().map(|s| s.settlement_time_ms),
            closed: self.exit.is_some(),
        })
    }

    fn validate(&self) -> Result<(), CarryError> {
        if self.size <= Decimal::ZERO {
            return Err(CarryError::NonPositiveSize { size: self.size });
        }
        require_positive("perp entry price", self.perp_entry_price)?;
        if let Some(spot) = self.spot_entry_price {
            require_positive("spot entry price", spot)?;
        }
        for rate in [self.fees.entry_rate, self.fees.exit_rate] {
            if rate < Decimal::ZERO {
                return Err(CarryError::NegativeFeeRate { rate });
            }
        }

        if let Some(exit) = &self.exit {
            require_positive("perp exit price", exit.perp_price)?;
            match (self.spot_entry_price, exit.spot_price) {
                (Some(_), Some(spot)) => require_positive("spot exit price", spot)?,
                (None, None) => {}
                _ => return Err(CarryError::HedgeMismatch),
            }
        }

        Ok(())
    }

    fn accrue_funding(&self, settlements: &[FundingSettlement]) -> Result<Decimal, CarryError> {
        let sign = self.side.funding_sign();
        let mut total = Decimal::ZERO;
        let mut previous_time: Option<i64> = None;

        for settlement in settlements {
            if previous_time.is_some_and(|previous| settlement.settlement_time_ms <= previous) {
                return Err(CarryError::UnorderedSettlements {
                    settlement_time_ms: settlement.settlement_time_ms,
                });
            }
            previous_time = Some(settlement.settlement_time_ms);

            require_positive("settlement mark price", settlement.mark_price)?;

            let notional = mul(self.size, settlement.mark_price)?;
            let payment = mul(mul(notional, settlement.funding_rate)?, sign)?;
            total = add(total, payment)?;
        }

        Ok(total)
    }

    /// Price result on each leg, or zeros while the trade is open.
    fn price_result(&self) -> Result<(Decimal, Decimal), CarryError> {
        let Some(exit) = &self.exit else {
            return Ok((Decimal::ZERO, Decimal::ZERO));
        };

        let perp_move = sub(exit.perp_price, self.perp_entry_price)?;
        let perp = mul(mul(self.size, perp_move)?, self.side.price_sign())?;

        let spot = match (self.spot_entry_price, exit.spot_price) {
            (Some(entry), Some(close)) => {
                // The hedge is held in the opposite direction to the perp leg.
                let spot_move = sub(close, entry)?;
                mul(mul(self.size, spot_move)?, -self.side.price_sign())?
            }
            _ => Decimal::ZERO,
        };

        Ok((perp, spot))
    }

    /// Total fees across every leg actually traded.
    fn fees(&self) -> Result<Decimal, CarryError> {
        let mut total = mul(mul(self.size, self.perp_entry_price)?, self.fees.entry_rate)?;

        if let Some(spot) = self.spot_entry_price {
            total = add(total, mul(mul(self.size, spot)?, self.fees.entry_rate)?)?;
        }

        if let Some(exit) = &self.exit {
            total = add(
                total,
                mul(mul(self.size, exit.perp_price)?, self.fees.exit_rate)?,
            )?;
            if let Some(spot) = exit.spot_price {
                total = add(total, mul(mul(self.size, spot)?, self.fees.exit_rate)?)?;
            }
        }

        Ok(total)
    }
}

/// Annualize a realized net result as a simple return on deployed capital.
///
/// This is the position-level counterpart to
/// [`funding_apr`](crate::metrics::funding_apr): that function annualizes a
/// quoted rate, while this one annualizes what a position actually kept. It is
/// a simple, non-compounding projection of one holding period onto a year, and
/// says nothing about whether the rate persists.
///
/// # Errors
///
/// Returns [`CarryError::NonPositivePrice`] when `capital` or `holding_hours`
/// is not strictly positive, and [`CarryError::Overflow`] on an out-of-range
/// result.
///
/// # Examples
///
/// ```
/// use hypercarry_core::carry::annualized_return;
/// use rust_decimal::Decimal;
///
/// // 1% kept over 24 hours, projected onto a 365-day year.
/// let apr = annualized_return(Decimal::ONE, Decimal::from(100), Decimal::from(24))?;
/// assert_eq!(apr, Decimal::new(3_65, 2));
/// # Ok::<(), hypercarry_core::carry::CarryError>(())
/// ```
pub fn annualized_return(
    net: Decimal,
    capital: Decimal,
    holding_hours: Decimal,
) -> Result<Decimal, CarryError> {
    require_positive("capital", capital)?;
    require_positive("holding hours", holding_hours)?;

    let period_return = net.checked_div(capital).ok_or(CarryError::Overflow)?;
    let periods_per_year = Decimal::from(HOURS_PER_YEAR)
        .checked_div(holding_hours)
        .ok_or(CarryError::Overflow)?;

    mul(period_return, periods_per_year)
}

fn require_positive(field: &'static str, price: Decimal) -> Result<(), CarryError> {
    if price <= Decimal::ZERO {
        return Err(CarryError::NonPositivePrice { field, price });
    }
    Ok(())
}

fn mul(a: Decimal, b: Decimal) -> Result<Decimal, CarryError> {
    a.checked_mul(b).ok_or(CarryError::Overflow)
}

fn add(a: Decimal, b: Decimal) -> Result<Decimal, CarryError> {
    a.checked_add(b).ok_or(CarryError::Overflow)
}

fn sub(a: Decimal, b: Decimal) -> Result<Decimal, CarryError> {
    a.checked_sub(b).ok_or(CarryError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(value: &str) -> Decimal {
        value.parse().expect("test decimal parses")
    }

    fn settlement(time_ms: i64, rate: &str, mark: &str) -> FundingSettlement {
        FundingSettlement {
            settlement_time_ms: time_ms,
            funding_rate: dec(rate),
            mark_price: dec(mark),
        }
    }

    fn hedged_short() -> CarryTrade {
        CarryTrade {
            side: CarrySide::Short,
            size: Decimal::ONE,
            perp_entry_price: dec("80000"),
            spot_entry_price: Some(dec("80000")),
            exit: None,
            fees: FeeSchedule::zero(),
        }
    }

    #[test]
    fn short_receives_and_long_pays_positive_funding() {
        let settlements = [settlement(1, "0.0000125", "80000")];

        let short = hedged_short()
            .evaluate(&settlements)
            .expect("short evaluates");
        let long = CarryTrade {
            side: CarrySide::Long,
            ..hedged_short()
        }
        .evaluate(&settlements)
        .expect("long evaluates");

        assert_eq!(short.funding, Decimal::ONE);
        assert_eq!(long.funding, -Decimal::ONE);
        assert_eq!(short.funding, -long.funding);
    }

    #[test]
    fn negative_rate_inverts_the_funding_direction() {
        let settlements = [settlement(1, "-0.0000125", "80000")];

        let pnl = hedged_short().evaluate(&settlements).expect("evaluates");

        assert_eq!(pnl.funding, -Decimal::ONE);
    }

    #[test]
    fn funding_accrues_across_every_settlement_at_its_own_mark() {
        let settlements = [
            settlement(1, "0.0001", "80000"),  // 8
            settlement(2, "0.0001", "90000"),  // 9
            settlement(3, "0.0001", "100000"), // 10
        ];

        let pnl = hedged_short().evaluate(&settlements).expect("evaluates");

        assert_eq!(pnl.funding, dec("27"));
        assert_eq!(pnl.settlements, 3);
        assert_eq!(pnl.first_settlement_ms, Some(1));
        assert_eq!(pnl.last_settlement_ms, Some(3));
    }

    #[test]
    fn hedged_trade_cancels_a_parallel_price_move() {
        let trade = CarryTrade {
            exit: Some(CarryExit {
                perp_price: dec("90000"),
                spot_price: Some(dec("90000")),
            }),
            ..hedged_short()
        };

        let pnl = trade.evaluate(&[]).expect("evaluates");

        assert_eq!(pnl.perp_price_pnl, dec("-10000"));
        assert_eq!(pnl.spot_price_pnl, dec("10000"));
        assert_eq!(pnl.net, Decimal::ZERO);
        assert!(pnl.closed);
    }

    #[test]
    fn hedged_trade_keeps_basis_convergence() {
        // Perp entered 500 rich to spot and converged; the short keeps the 500.
        let trade = CarryTrade {
            perp_entry_price: dec("80500"),
            spot_entry_price: Some(dec("80000")),
            exit: Some(CarryExit {
                perp_price: dec("90000"),
                spot_price: Some(dec("90000")),
            }),
            ..hedged_short()
        };

        let pnl = trade.evaluate(&[]).expect("evaluates");

        assert_eq!(pnl.net, dec("500"));
    }

    #[test]
    fn unhedged_trade_keeps_its_directional_result() {
        let trade = CarryTrade {
            spot_entry_price: None,
            exit: Some(CarryExit {
                perp_price: dec("90000"),
                spot_price: None,
            }),
            ..hedged_short()
        };

        let pnl = trade.evaluate(&[]).expect("evaluates");

        assert_eq!(pnl.perp_price_pnl, dec("-10000"));
        assert_eq!(pnl.spot_price_pnl, Decimal::ZERO);
        assert_eq!(pnl.net, dec("-10000"));
    }

    #[test]
    fn open_trade_reports_funding_only() {
        let settlements = [settlement(1, "0.0001", "80000")];

        let pnl = hedged_short().evaluate(&settlements).expect("evaluates");

        assert_eq!(pnl.perp_price_pnl, Decimal::ZERO);
        assert_eq!(pnl.spot_price_pnl, Decimal::ZERO);
        assert_eq!(pnl.net, pnl.funding);
        assert!(!pnl.closed);
    }

    #[test]
    fn fees_charge_every_leg_of_the_round_trip() {
        let trade = CarryTrade {
            exit: Some(CarryExit {
                perp_price: dec("80000"),
                spot_price: Some(dec("80000")),
            }),
            fees: FeeSchedule::flat(dec("0.0001")),
            ..hedged_short()
        };

        let pnl = trade.evaluate(&[]).expect("evaluates");

        // Four charges of 80_000 * 0.0001.
        assert_eq!(pnl.fees, dec("32"));
        assert_eq!(pnl.net, dec("-32"));
    }

    #[test]
    fn open_hedged_trade_charges_entry_fees_only() {
        let trade = CarryTrade {
            fees: FeeSchedule::flat(dec("0.0001")),
            ..hedged_short()
        };

        let pnl = trade.evaluate(&[]).expect("evaluates");

        assert_eq!(pnl.fees, dec("16"));
    }

    #[test]
    fn components_sum_exactly_to_net() {
        let trade = CarryTrade {
            perp_entry_price: dec("80500"),
            exit: Some(CarryExit {
                perp_price: dec("90000"),
                spot_price: Some(dec("89750")),
            }),
            fees: FeeSchedule::flat(dec("0.00045")),
            ..hedged_short()
        };
        let settlements = [
            settlement(1, "0.0000125", "80000"),
            settlement(2, "0.00002", "85000"),
        ];

        let pnl = trade.evaluate(&settlements).expect("evaluates");

        assert_eq!(
            pnl.net,
            pnl.funding + pnl.perp_price_pnl + pnl.spot_price_pnl - pnl.fees
        );
    }

    #[test]
    fn entry_notional_reports_the_perpetual_leg() {
        let trade = CarryTrade {
            size: dec("2.5"),
            ..hedged_short()
        };

        let pnl = trade.evaluate(&[]).expect("evaluates");

        assert_eq!(pnl.entry_notional, dec("200000"));
    }

    #[test]
    fn repeated_or_out_of_order_settlements_fail_closed() {
        let repeated = [
            settlement(1_000, "0.0001", "80000"),
            settlement(1_000, "0.0001", "80000"),
        ];
        let reversed = [
            settlement(2_000, "0.0001", "80000"),
            settlement(1_000, "0.0001", "80000"),
        ];

        for settlements in [repeated, reversed] {
            assert_eq!(
                hedged_short().evaluate(&settlements),
                Err(CarryError::UnorderedSettlements {
                    settlement_time_ms: 1_000
                })
            );
        }
    }

    #[test]
    fn non_positive_size_is_rejected() {
        for size in [Decimal::ZERO, -Decimal::ONE] {
            let trade = CarryTrade {
                size,
                ..hedged_short()
            };
            assert_eq!(
                trade.evaluate(&[]),
                Err(CarryError::NonPositiveSize { size })
            );
        }
    }

    #[test]
    fn non_positive_prices_are_rejected_with_their_field() {
        let trade = CarryTrade {
            perp_entry_price: Decimal::ZERO,
            ..hedged_short()
        };
        assert_eq!(
            trade.evaluate(&[]),
            Err(CarryError::NonPositivePrice {
                field: "perp entry price",
                price: Decimal::ZERO
            })
        );

        let settlements = [settlement(1, "0.0001", "0")];
        assert_eq!(
            hedged_short().evaluate(&settlements),
            Err(CarryError::NonPositivePrice {
                field: "settlement mark price",
                price: Decimal::ZERO
            })
        );
    }

    #[test]
    fn negative_fee_rate_is_rejected() {
        let trade = CarryTrade {
            fees: FeeSchedule::flat(dec("-0.0001")),
            ..hedged_short()
        };

        assert_eq!(
            trade.evaluate(&[]),
            Err(CarryError::NegativeFeeRate {
                rate: dec("-0.0001")
            })
        );
    }

    #[test]
    fn hedge_must_be_described_consistently() {
        let unclosed_hedge = CarryTrade {
            exit: Some(CarryExit {
                perp_price: dec("80000"),
                spot_price: None,
            }),
            ..hedged_short()
        };
        let unopened_hedge = CarryTrade {
            spot_entry_price: None,
            exit: Some(CarryExit {
                perp_price: dec("80000"),
                spot_price: Some(dec("80000")),
            }),
            ..hedged_short()
        };

        for trade in [unclosed_hedge, unopened_hedge] {
            assert_eq!(trade.evaluate(&[]), Err(CarryError::HedgeMismatch));
        }
    }

    #[test]
    fn extreme_values_overflow_instead_of_panicking() {
        let trade = CarryTrade {
            size: Decimal::MAX,
            perp_entry_price: Decimal::MAX,
            spot_entry_price: None,
            exit: None,
            ..hedged_short()
        };

        assert_eq!(trade.evaluate(&[]), Err(CarryError::Overflow));
    }

    #[test]
    fn annualized_return_projects_a_holding_period_onto_a_year() {
        assert_eq!(
            annualized_return(Decimal::ONE, dec("100"), dec("24")).expect("annualizes"),
            dec("3.65")
        );
        assert_eq!(
            annualized_return(Decimal::ONE, dec("100"), Decimal::from(HOURS_PER_YEAR))
                .expect("annualizes"),
            dec("0.01")
        );
    }

    #[test]
    fn annualized_return_rejects_non_positive_inputs() {
        assert_eq!(
            annualized_return(Decimal::ONE, Decimal::ZERO, dec("24")),
            Err(CarryError::NonPositivePrice {
                field: "capital",
                price: Decimal::ZERO
            })
        );
        assert_eq!(
            annualized_return(Decimal::ONE, dec("100"), Decimal::ZERO),
            Err(CarryError::NonPositivePrice {
                field: "holding hours",
                price: Decimal::ZERO
            })
        );
    }
}
