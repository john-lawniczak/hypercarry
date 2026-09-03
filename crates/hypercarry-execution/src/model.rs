use crate::{EXECUTION_SCHEMA_VERSION, ExecutionError};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Buy or sell direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// A buy (long) order.
    Buy,
    /// A sell (short) order.
    Sell,
}

/// Inert order proposal. Construction performs only strategy-independent checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OrderIntent {
    /// Version of the execution value contract.
    pub schema_version: u32,
    /// Caller-supplied correlation identifier for tracing.
    pub correlation_id: String,
    /// Target venue name.
    pub venue: String,
    /// Target market symbol.
    pub market: String,
    /// Buy or sell direction.
    pub side: Side,
    /// Requested order size in base-asset units.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Limit price in quote units.
    #[serde(with = "rust_decimal::serde::str")]
    pub limit_price: Decimal,
    /// Intent creation time, unix milliseconds.
    pub created_at_ms: i64,
}

impl OrderIntent {
    /// Creates a limit-order intent without resolving venue metadata.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identity, non-positive values, or a
    /// pre-epoch timestamp.
    pub fn limit(
        correlation_id: impl Into<String>,
        venue: impl Into<String>,
        market: impl Into<String>,
        side: Side,
        quantity: Decimal,
        limit_price: Decimal,
        created_at_ms: i64,
    ) -> Result<Self, ExecutionError> {
        let value = Self {
            schema_version: EXECUTION_SCHEMA_VERSION,
            correlation_id: correlation_id.into(),
            venue: venue.into(),
            market: market.into(),
            side,
            quantity,
            limit_price,
            created_at_ms,
        };
        value.validate()?;
        Ok(value)
    }

    /// Revalidates an intent after any caller mutation.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported schema or invalid field.
    pub fn validate(&self) -> Result<(), ExecutionError> {
        if self.schema_version != EXECUTION_SCHEMA_VERSION {
            return Err(ExecutionError::Validation(format!(
                "unsupported intent schema version {}",
                self.schema_version
            )));
        }
        validate_correlation_id(&self.correlation_id)?;
        validate_name("venue", &self.venue)?;
        validate_name("market", &self.market)?;
        positive("quantity", self.quantity)?;
        positive("limit price", self.limit_price)?;
        if self.created_at_ms < 0 {
            return Err(ExecutionError::Validation(
                "created_at_ms must not precede the Unix epoch".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Venue rules required to resolve an inert intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketMetadata {
    /// Venue this metadata applies to.
    pub venue: String,
    /// Market symbol this metadata applies to.
    pub market: String,
    /// Minimum price increment.
    #[serde(with = "rust_decimal::serde::str")]
    pub price_tick: Decimal,
    /// Minimum size increment.
    #[serde(with = "rust_decimal::serde::str")]
    pub size_step: Decimal,
    /// Minimum permitted order size.
    #[serde(with = "rust_decimal::serde::str")]
    pub minimum_size: Decimal,
}

impl MarketMetadata {
    /// Creates positive price/size increments for a named venue market.
    ///
    /// # Errors
    ///
    /// Returns an error for empty names or non-positive increments/minimums.
    pub fn new(
        venue: impl Into<String>,
        market: impl Into<String>,
        price_tick: Decimal,
        size_step: Decimal,
        minimum_size: Decimal,
    ) -> Result<Self, ExecutionError> {
        let value = Self {
            venue: venue.into(),
            market: market.into(),
            price_tick,
            size_step,
            minimum_size,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn validate(&self) -> Result<(), ExecutionError> {
        validate_name("venue", &self.venue).map_err(|error| metadata_error(&error))?;
        validate_name("market", &self.market).map_err(|error| metadata_error(&error))?;
        positive("price tick", self.price_tick).map_err(|error| metadata_error(&error))?;
        positive("size step", self.size_step).map_err(|error| metadata_error(&error))?;
        positive("minimum size", self.minimum_size).map_err(|error| metadata_error(&error))
    }
}

/// Fully quantized order accepted by the shared adapter boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedOrder {
    /// Version of the execution value contract.
    pub schema_version: u32,
    /// Correlation identifier carried from the originating intent.
    pub correlation_id: String,
    /// Target venue name.
    pub venue: String,
    /// Target market symbol.
    pub market: String,
    /// Buy or sell direction.
    pub side: Side,
    /// Quantized order size in base-asset units.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Quantized limit price in quote units.
    #[serde(with = "rust_decimal::serde::str")]
    pub limit_price: Decimal,
    /// Originating intent creation time, unix milliseconds.
    pub created_at_ms: i64,
}

impl ValidatedOrder {
    /// Resolves venue metadata and quantizes size and price conservatively.
    ///
    /// # Errors
    ///
    /// Returns an error for mismatched metadata, invalid intent values, or a
    /// quantity that falls below the venue minimum after quantization.
    pub fn resolve(
        intent: &OrderIntent,
        metadata: &MarketMetadata,
    ) -> Result<Self, ExecutionError> {
        intent.validate()?;
        metadata.validate()?;
        if intent.venue != metadata.venue || intent.market != metadata.market {
            return Err(ExecutionError::Metadata(format!(
                "resolved {}:{} for intent {}:{}",
                metadata.venue, metadata.market, intent.venue, intent.market
            )));
        }
        let quantity = quantize_down(intent.quantity, metadata.size_step);
        if quantity < metadata.minimum_size {
            return Err(ExecutionError::Validation(format!(
                "quantized quantity {quantity} is below minimum {}",
                metadata.minimum_size
            )));
        }
        let limit_price = match intent.side {
            Side::Buy => quantize_down(intent.limit_price, metadata.price_tick),
            Side::Sell => quantize_up(intent.limit_price, metadata.price_tick)?,
        };
        positive("quantized limit price", limit_price)?;
        let order = Self {
            schema_version: EXECUTION_SCHEMA_VERSION,
            correlation_id: intent.correlation_id.clone(),
            venue: intent.venue.clone(),
            market: intent.market.clone(),
            side: intent.side,
            quantity,
            limit_price,
            created_at_ms: intent.created_at_ms,
        };
        order.validate()?;
        Ok(order)
    }

    /// Revalidates a value before it crosses an adapter or journal boundary.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schema or invalid identity, price,
    /// quantity, or timestamp fields.
    pub fn validate(&self) -> Result<(), ExecutionError> {
        if self.schema_version != EXECUTION_SCHEMA_VERSION {
            return Err(ExecutionError::Validation(format!(
                "unsupported validated-order schema version {}",
                self.schema_version
            )));
        }
        validate_correlation_id(&self.correlation_id)?;
        validate_name("venue", &self.venue)?;
        validate_name("market", &self.market)?;
        positive("quantity", self.quantity)?;
        positive("limit price", self.limit_price)?;
        if self.created_at_ms < 0 {
            return Err(ExecutionError::Validation(
                "created_at_ms must not precede the Unix epoch".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Adapter operating mode recorded before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Deterministic local simulation.
    Simulation,
    /// Non-signing dry run that journals but never submits.
    DryRun,
    /// Live testnet execution.
    Testnet,
}

/// Venue-neutral lifecycle observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    /// Validated but not yet submitted.
    Validated,
    /// Submission is in flight.
    SubmissionPending,
    /// Submission outcome is unknown and needs reconciliation.
    SubmissionUncertain,
    /// Resting on the venue book.
    Open,
    /// Recorded by the dry-run adapter without submission.
    DryRunRecorded,
    /// Partially filled and still resting.
    PartiallyFilled,
    /// Cancellation is in flight.
    CancelPending,
    /// Fully filled.
    Filled,
    /// Cancelled with no further fills.
    Cancelled,
    /// Rejected by policy or the venue.
    Rejected,
    /// Completed with no fill.
    NoFill,
}

/// Deterministic simulated fill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fill {
    /// Fill time, unix milliseconds.
    pub occurred_at_ms: i64,
    /// Filled quantity in base-asset units.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Fill price in quote units.
    #[serde(with = "rust_decimal::serde::str")]
    pub price: Decimal,
    /// Fee charged for the fill, in quote units.
    #[serde(with = "rust_decimal::serde::str")]
    pub fee_quote: Decimal,
}

/// Cancellation result, including quantity left unfilled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancellation {
    /// Cancellation time, unix milliseconds.
    pub occurred_at_ms: i64,
    /// Quantity still unfilled at cancellation.
    #[serde(with = "rust_decimal::serde::str")]
    pub remaining_quantity: Decimal,
}

/// Structured policy or venue rejection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    /// Stable machine-readable rejection code.
    pub code: String,
    /// Human-readable rejection reason.
    pub reason: String,
}

/// Complete adapter result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReport {
    /// Version of the execution value contract.
    pub schema_version: u32,
    /// Correlation identifier carried from the order.
    pub correlation_id: String,
    /// Mode the adapter ran in.
    pub mode: ExecutionMode,
    /// Terminal or interim lifecycle state.
    pub state: OrderState,
    /// Fills applied to the order.
    pub fills: Vec<Fill>,
    /// Cancellation outcome, when the order was cancelled.
    pub cancellation: Option<Cancellation>,
    /// Rejection detail, when the order was rejected.
    pub rejection: Option<Rejection>,
}

impl ExecutionReport {
    pub(crate) fn empty(order: &ValidatedOrder, mode: ExecutionMode, state: OrderState) -> Self {
        Self {
            schema_version: EXECUTION_SCHEMA_VERSION,
            correlation_id: order.correlation_id.clone(),
            mode,
            state,
            fills: Vec::new(),
            cancellation: None,
            rejection: None,
        }
    }
}

pub(crate) fn validate_correlation_id(value: &str) -> Result<(), ExecutionError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ExecutionError::Validation(
            "correlation ID must be 1..=128 ASCII letters, digits, '-', '_', or '.'".to_owned(),
        ));
    }
    Ok(())
}

fn validate_name(field: &str, value: &str) -> Result<(), ExecutionError> {
    if value.trim().is_empty() {
        return Err(ExecutionError::Validation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn positive(field: &str, value: Decimal) -> Result<(), ExecutionError> {
    if value <= Decimal::ZERO {
        return Err(ExecutionError::Validation(format!(
            "{field} must be positive, got {value}"
        )));
    }
    Ok(())
}

fn metadata_error(error: &ExecutionError) -> ExecutionError {
    ExecutionError::Metadata(error.to_string())
}

fn quantize_down(value: Decimal, increment: Decimal) -> Decimal {
    value - value % increment
}

fn quantize_up(value: Decimal, increment: Decimal) -> Result<Decimal, ExecutionError> {
    let remainder = value % increment;
    if remainder.is_zero() {
        return Ok(value);
    }
    value
        .checked_add(increment)
        .and_then(|sum| sum.checked_sub(remainder))
        .ok_or_else(|| {
            ExecutionError::Validation(format!(
                "quantizing {value} up to increment {increment} overflows the representable range"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_quantizes_buy_down_sell_up_and_size_down() {
        let metadata =
            MarketMetadata::new("venue", "BTC-PERP", dec("0.5"), dec("0.01"), dec("0.1")).unwrap();
        let buy = OrderIntent::limit(
            "buy-1",
            "venue",
            "BTC-PERP",
            Side::Buy,
            dec("1.239"),
            dec("101.24"),
            1,
        )
        .unwrap();
        let sell = OrderIntent::limit(
            "sell-1",
            "venue",
            "BTC-PERP",
            Side::Sell,
            dec("1.239"),
            dec("101.24"),
            1,
        )
        .unwrap();

        assert_eq!(
            ValidatedOrder::resolve(&buy, &metadata).unwrap().quantity,
            dec("1.23")
        );
        assert_eq!(
            ValidatedOrder::resolve(&buy, &metadata)
                .unwrap()
                .limit_price,
            dec("101")
        );
        assert_eq!(
            ValidatedOrder::resolve(&sell, &metadata)
                .unwrap()
                .limit_price,
            dec("101.5")
        );
    }

    #[test]
    fn resolution_fails_closed_instead_of_panicking_when_sell_quantization_overflows() {
        // Decimal::MAX % 11 == 8 (nonzero), so quantize_up must add the tick
        // and subtract the remainder; `Decimal::MAX + 11` alone already
        // exceeds the representable range.
        let metadata =
            MarketMetadata::new("venue", "BTC-PERP", dec("11"), dec("0.01"), dec("0.01")).unwrap();
        let sell = OrderIntent::limit(
            "sell-overflow",
            "venue",
            "BTC-PERP",
            Side::Sell,
            dec("1"),
            Decimal::MAX,
            1,
        )
        .unwrap();

        let error = ValidatedOrder::resolve(&sell, &metadata).unwrap_err();
        assert!(error.to_string().contains("overflows"));
    }

    #[test]
    fn resolution_rejects_quantity_that_quantizes_below_minimum() {
        let metadata =
            MarketMetadata::new("venue", "BTC-PERP", dec("1"), dec("0.1"), dec("0.5")).unwrap();
        let intent = OrderIntent::limit(
            "edge-1",
            "venue",
            "BTC-PERP",
            Side::Buy,
            dec("0.49"),
            dec("100"),
            1,
        )
        .unwrap();
        let error = ValidatedOrder::resolve(&intent, &metadata).unwrap_err();
        assert!(error.to_string().contains("below minimum"));
    }

    #[test]
    fn resolution_revalidates_mutated_metadata_before_decimal_remainder() {
        let mut metadata =
            MarketMetadata::new("venue", "BTC-PERP", dec("1"), dec("0.1"), dec("0.1")).unwrap();
        metadata.size_step = Decimal::ZERO;
        let intent = OrderIntent::limit(
            "edge-2",
            "venue",
            "BTC-PERP",
            Side::Buy,
            dec("1"),
            dec("100"),
            1,
        )
        .unwrap();

        let error = ValidatedOrder::resolve(&intent, &metadata).unwrap_err();
        assert!(error.to_string().contains("size step must be positive"));
    }

    fn dec(value: &str) -> Decimal {
        value.parse().unwrap()
    }
}
