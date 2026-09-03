use crate::{
    Cancellation, ExecutionAdapter, ExecutionError, ExecutionMode, ExecutionReport, Fill,
    OrderState, Side, ValidatedOrder,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

const BPS_DENOMINATOR: u32 = 10_000;

/// Deterministic top-of-book replay observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketFrame {
    /// Observation time, unix milliseconds.
    pub observed_at_ms: i64,
    /// Best bid price.
    #[serde(with = "rust_decimal::serde::str")]
    pub best_bid: Decimal,
    /// Best ask price.
    #[serde(with = "rust_decimal::serde::str")]
    pub best_ask: Decimal,
    /// Size available at the best bid.
    #[serde(with = "rust_decimal::serde::str")]
    pub bid_size: Decimal,
    /// Size available at the best ask.
    #[serde(with = "rust_decimal::serde::str")]
    pub ask_size: Decimal,
}

impl MarketFrame {
    /// Creates a validated top-of-book frame.
    ///
    /// # Errors
    ///
    /// Returns an error for pre-epoch time, crossed/non-positive prices, or
    /// negative available size.
    pub fn new(
        observed_at_ms: i64,
        best_bid: Decimal,
        best_ask: Decimal,
        bid_size: Decimal,
        ask_size: Decimal,
    ) -> Result<Self, ExecutionError> {
        let frame = Self {
            observed_at_ms,
            best_bid,
            best_ask,
            bid_size,
            ask_size,
        };
        frame.validate()?;
        Ok(frame)
    }

    fn validate(&self) -> Result<(), ExecutionError> {
        if self.observed_at_ms < 0 {
            return Err(simulation_error("frame time precedes the Unix epoch"));
        }
        if self.best_bid <= Decimal::ZERO
            || self.best_ask <= Decimal::ZERO
            || self.best_bid > self.best_ask
        {
            return Err(simulation_error(
                "frame requires positive prices with best_bid <= best_ask",
            ));
        }
        if self.bid_size < Decimal::ZERO || self.ask_size < Decimal::ZERO {
            return Err(simulation_error("frame sizes must not be negative"));
        }
        Ok(())
    }
}

/// Which event wins when a fill and cancellation share a timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelRacePriority {
    /// The cancellation is applied before the same-timestamp fill.
    CancelWins,
    /// The fill is applied before the same-timestamp cancellation.
    FillWins,
}

/// Replay assumptions. Fee and slippage values are non-negative basis points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulationConfig {
    /// Simulated round-trip latency, milliseconds.
    pub latency_ms: u64,
    /// Taker fee applied to fills, basis points.
    pub fee_bps: Decimal,
    /// Adverse slippage applied to fills, basis points.
    pub slippage_bps: Decimal,
    /// Tie-break rule for same-timestamp fill/cancel races.
    pub cancel_race_priority: CancelRacePriority,
}

impl SimulationConfig {
    /// Creates deterministic execution assumptions.
    ///
    /// # Errors
    ///
    /// Returns an error when fees are negative or slippage is outside
    /// `[0, 10000)` basis points.
    pub fn new(
        latency_ms: u64,
        fee_bps: Decimal,
        slippage_bps: Decimal,
        cancel_race_priority: CancelRacePriority,
    ) -> Result<Self, ExecutionError> {
        if fee_bps < Decimal::ZERO
            || slippage_bps < Decimal::ZERO
            || slippage_bps >= Decimal::from(BPS_DENOMINATOR)
        {
            return Err(simulation_error(
                "fee basis points must not be negative and slippage must be in [0, 10000)",
            ));
        }
        Ok(Self {
            latency_ms,
            fee_bps,
            slippage_bps,
            cancel_race_priority,
        })
    }
}

/// Local adapter that replays validated orders against deterministic frames.
pub struct SimulatorAdapter {
    frames: Vec<MarketFrame>,
    config: SimulationConfig,
    cancellation_at_ms: Option<i64>,
}

impl SimulatorAdapter {
    /// Creates a simulator over time-ordered frames.
    ///
    /// # Errors
    ///
    /// Returns an error for out-of-order frames or a pre-epoch cancellation.
    pub fn new(
        frames: Vec<MarketFrame>,
        config: SimulationConfig,
        cancellation_at_ms: Option<i64>,
    ) -> Result<Self, ExecutionError> {
        if frames
            .windows(2)
            .any(|pair| pair[0].observed_at_ms > pair[1].observed_at_ms)
        {
            return Err(simulation_error(
                "market frames must be ordered by observed_at_ms",
            ));
        }
        for frame in &frames {
            frame.validate()?;
        }
        if config.fee_bps < Decimal::ZERO
            || config.slippage_bps < Decimal::ZERO
            || config.slippage_bps >= Decimal::from(BPS_DENOMINATOR)
        {
            return Err(simulation_error("simulation config is invalid"));
        }
        if cancellation_at_ms.is_some_and(|timestamp| timestamp < 0) {
            return Err(simulation_error(
                "cancellation time must not precede the Unix epoch",
            ));
        }
        Ok(Self {
            frames,
            config,
            cancellation_at_ms,
        })
    }

    fn simulate(&self, order: &ValidatedOrder) -> Result<ExecutionReport, ExecutionError> {
        order.validate()?;
        let latency_ms = i64::try_from(self.config.latency_ms)
            .map_err(|_| simulation_error("latency does not fit signed milliseconds"))?;
        let eligible_at_ms = order
            .created_at_ms
            .checked_add(latency_ms)
            .ok_or_else(|| simulation_error("order eligibility timestamp overflow"))?;
        let mut report =
            ExecutionReport::empty(order, ExecutionMode::Simulation, OrderState::NoFill);
        let mut remaining = order.quantity;

        for frame in self
            .frames
            .iter()
            .filter(|frame| frame.observed_at_ms >= eligible_at_ms)
        {
            if let Some(cancelled_at_ms) = self.winning_cancellation(frame.observed_at_ms) {
                report.state = OrderState::Cancelled;
                report.cancellation = Some(Cancellation {
                    occurred_at_ms: cancelled_at_ms,
                    remaining_quantity: remaining,
                });
                return Ok(report);
            }
            let (reference_price, available) = match order.side {
                Side::Buy => (frame.best_ask, frame.ask_size),
                Side::Sell => (frame.best_bid, frame.bid_size),
            };
            let execution_price =
                apply_slippage(reference_price, order.side, self.config.slippage_bps)?;
            if !crosses_limit(order.side, execution_price, order.limit_price) || available.is_zero()
            {
                continue;
            }
            let fill_quantity = remaining.min(available);
            let fee_quote = (fill_quantity * execution_price * self.config.fee_bps
                / Decimal::from(BPS_DENOMINATOR))
            .normalize();
            report.fills.push(Fill {
                occurred_at_ms: frame.observed_at_ms,
                quantity: fill_quantity,
                price: execution_price.normalize(),
                fee_quote,
            });
            remaining -= fill_quantity;
            if remaining.is_zero() {
                report.state = OrderState::Filled;
                return Ok(report);
            }
            report.state = OrderState::PartiallyFilled;
        }

        if let Some(cancelled_at_ms) = self.cancellation_at_ms {
            report.state = OrderState::Cancelled;
            report.cancellation = Some(Cancellation {
                occurred_at_ms: cancelled_at_ms,
                remaining_quantity: remaining,
            });
        }
        Ok(report)
    }

    fn winning_cancellation(&self, frame_at_ms: i64) -> Option<i64> {
        self.cancellation_at_ms.filter(|cancelled_at_ms| {
            *cancelled_at_ms < frame_at_ms
                || (*cancelled_at_ms == frame_at_ms
                    && self.config.cancel_race_priority == CancelRacePriority::CancelWins)
        })
    }
}

impl ExecutionAdapter for SimulatorAdapter {
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Simulation
    }

    fn execute(&mut self, order: &ValidatedOrder) -> Result<ExecutionReport, ExecutionError> {
        self.simulate(order)
    }
}

fn crosses_limit(side: Side, execution_price: Decimal, limit_price: Decimal) -> bool {
    match side {
        Side::Buy => execution_price <= limit_price,
        Side::Sell => execution_price >= limit_price,
    }
}

fn apply_slippage(
    price: Decimal,
    side: Side,
    slippage_bps: Decimal,
) -> Result<Decimal, ExecutionError> {
    let fraction = slippage_bps / Decimal::from(BPS_DENOMINATOR);
    let slipped = match side {
        Side::Buy => price * (Decimal::ONE + fraction),
        Side::Sell => price * (Decimal::ONE - fraction),
    };
    if slipped <= Decimal::ZERO {
        return Err(simulation_error(
            "configured slippage produces a non-positive execution price",
        ));
    }
    Ok(slipped)
}

fn simulation_error(message: impl Into<String>) -> ExecutionError {
    ExecutionError::Simulation(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MarketMetadata, OrderIntent};

    #[test]
    fn simulator_applies_latency_slippage_fees_and_partial_fills() {
        let frames = vec![
            frame(1_050, "99", "100", "2", "0.4"),
            frame(1_100, "99", "100", "2", "0.8"),
            frame(1_200, "99", "100", "2", "1"),
        ];
        let config =
            SimulationConfig::new(100, dec("5"), dec("10"), CancelRacePriority::CancelWins)
                .unwrap();
        let mut simulator = SimulatorAdapter::new(frames, config, None).unwrap();
        let report = simulator.execute(&order(Side::Buy, "1.5", "101")).unwrap();

        assert_eq!(report.state, OrderState::Filled);
        assert_eq!(report.fills.len(), 2);
        assert_eq!(report.fills[0].occurred_at_ms, 1_100);
        assert_eq!(report.fills[0].price, dec("100.1"));
        assert_eq!(report.fills[0].fee_quote, dec("0.04004"));
        assert_eq!(report.fills[1].quantity, dec("0.7"));
    }

    #[test]
    fn simulator_models_cancel_race_priority_and_no_fill() {
        let market_frame = frame(1_100, "99", "100", "1", "1");
        let cancel_config = SimulationConfig::new(
            100,
            Decimal::ZERO,
            Decimal::ZERO,
            CancelRacePriority::CancelWins,
        )
        .unwrap();
        let fill_config = SimulationConfig::new(
            100,
            Decimal::ZERO,
            Decimal::ZERO,
            CancelRacePriority::FillWins,
        )
        .unwrap();
        let mut cancel =
            SimulatorAdapter::new(vec![market_frame.clone()], cancel_config, Some(1_100)).unwrap();
        let mut fill = SimulatorAdapter::new(vec![market_frame], fill_config, Some(1_100)).unwrap();
        assert_eq!(
            cancel.execute(&order(Side::Buy, "1", "100")).unwrap().state,
            OrderState::Cancelled
        );
        assert_eq!(
            fill.execute(&order(Side::Buy, "1", "100")).unwrap().state,
            OrderState::Filled
        );

        let mut no_fill = SimulatorAdapter::new(
            vec![frame(1_100, "101", "102", "1", "1")],
            SimulationConfig::new(
                0,
                Decimal::ZERO,
                Decimal::ZERO,
                CancelRacePriority::CancelWins,
            )
            .unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(
            no_fill
                .execute(&order(Side::Buy, "1", "100"))
                .unwrap()
                .state,
            OrderState::NoFill
        );
    }

    #[test]
    fn simulator_rejects_unsorted_frames() {
        let error = SimulatorAdapter::new(
            vec![
                frame(2_000, "99", "100", "1", "1"),
                frame(1_000, "99", "100", "1", "1"),
            ],
            SimulationConfig::new(
                0,
                Decimal::ZERO,
                Decimal::ZERO,
                CancelRacePriority::CancelWins,
            )
            .unwrap(),
            None,
        )
        .err()
        .expect("unsorted replay must fail closed");
        assert!(error.to_string().contains("must be ordered"));
    }

    #[test]
    fn simulator_rejects_slippage_that_can_make_sell_price_non_positive() {
        let error = SimulationConfig::new(
            0,
            Decimal::ZERO,
            dec("10000"),
            CancelRacePriority::CancelWins,
        )
        .unwrap_err();
        assert!(error.to_string().contains("[0, 10000)"));
    }

    fn order(side: Side, quantity: &str, price: &str) -> ValidatedOrder {
        let metadata =
            MarketMetadata::new("venue", "BTC-PERP", dec("0.1"), dec("0.1"), dec("0.1")).unwrap();
        let intent = OrderIntent::limit(
            "intent-1",
            "venue",
            "BTC-PERP",
            side,
            dec(quantity),
            dec(price),
            1_000,
        )
        .unwrap();
        ValidatedOrder::resolve(&intent, &metadata).unwrap()
    }

    fn frame(time: i64, bid: &str, ask: &str, bid_size: &str, ask_size: &str) -> MarketFrame {
        MarketFrame::new(time, dec(bid), dec(ask), dec(bid_size), dec(ask_size)).unwrap()
    }

    fn dec(value: &str) -> Decimal {
        value.parse().unwrap()
    }
}
