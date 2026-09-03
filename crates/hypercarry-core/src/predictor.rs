//! Deterministic Hyperliquid next-hour funding prediction and evaluation.
//!
//! The baseline mirrors Hyperliquid's documented premium calculation and
//! funding formula. Samples are collapsed into UTC-aligned five-second slots,
//! so repeated recorder updates cannot overweight one protocol sampling slot.

use crate::types::{TimestampMs, VenuePrediction};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    fmt,
    num::NonZeroUsize,
    str::FromStr,
};

/// Stable JSON contract version for predictions and evaluations.
pub const PREDICTION_SCHEMA_VERSION: u32 = 1;
/// Hyperliquid samples the premium once every five seconds.
pub const PREMIUM_SAMPLE_INTERVAL_MS: i64 = 5_000;
/// Number of five-second sampling slots in one hour.
pub const PREMIUM_SAMPLES_PER_HOUR: u32 = 720;
/// UTC funding-window duration in milliseconds.
pub const FUNDING_HOUR_MS: i64 = 3_600_000;

fn default_impact_notional(coin: &str) -> Decimal {
    if matches!(coin, "BTC" | "ETH") {
        Decimal::from(20_000_u32)
    } else {
        Decimal::from(6_000_u32)
    }
}

fn interest_rate_eight_hour() -> Decimal {
    Decimal::new(1, 4)
}

fn clamp_bound_eight_hour() -> Decimal {
    Decimal::new(5, 4)
}

fn hourly_funding_cap() -> Decimal {
    Decimal::new(4, 2)
}

fn eight() -> Decimal {
    Decimal::from(8_u32)
}

/// One timestamped premium input reconstructed from an asset-context update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PremiumSample {
    /// Exact perpetual symbol.
    pub coin: String,
    /// Local receive time in Unix milliseconds.
    pub observed_at_ms: TimestampMs,
    /// Oracle price used by the premium formula.
    #[serde(with = "rust_decimal::serde::str")]
    pub oracle_px: Decimal,
    /// Average execution price for selling the configured impact notional.
    #[serde(with = "rust_decimal::serde::str")]
    pub impact_bid_px: Decimal,
    /// Average execution price for buying the configured impact notional.
    #[serde(with = "rust_decimal::serde::str")]
    pub impact_ask_px: Decimal,
    /// Premium reported by Hyperliquid, retained for diagnostics only.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub reported_premium: Option<Decimal>,
}

impl PremiumSample {
    /// Construct and validate a premium sample.
    ///
    /// # Errors
    ///
    /// Rejects an empty coin or non-positive oracle/impact prices.
    pub fn new(
        coin: impl Into<String>,
        observed_at_ms: TimestampMs,
        oracle_px: Decimal,
        impact_bid_px: Decimal,
        impact_ask_px: Decimal,
        reported_premium: Option<Decimal>,
    ) -> Result<Self, PredictorError> {
        let coin = coin.into();
        if coin.trim().is_empty() {
            return Err(PredictorError::EmptyCoin);
        }
        for (field, value) in [
            ("oracle_px", oracle_px),
            ("impact_bid_px", impact_bid_px),
            ("impact_ask_px", impact_ask_px),
        ] {
            if value <= Decimal::ZERO {
                return Err(PredictorError::NonPositivePrice { field, value });
            }
        }
        Ok(Self {
            coin,
            observed_at_ms,
            oracle_px,
            impact_bid_px,
            impact_ask_px,
            reported_premium,
        })
    }

    /// Parse a recorder WebSocket payload into a sample when it is an
    /// `activeAssetCtx` update with live impact prices.
    ///
    /// Unsupported channels and contexts with missing or explicit `null`
    /// impact prices return `Ok(None)`. Malformed subscribed contexts return a
    /// typed error.
    ///
    /// # Errors
    ///
    /// Returns [`PredictorError::Payload`] for malformed JSON or fields and the
    /// normal sample validation errors for invalid prices.
    pub fn from_asset_context_payload(
        payload: &str,
        observed_at_ms: TimestampMs,
    ) -> Result<Option<Self>, PredictorError> {
        let value: Value = serde_json::from_str(payload)
            .map_err(|source| PredictorError::Payload(format!("invalid JSON: {source}")))?;
        let Some(channel) = value.get("channel").and_then(Value::as_str) else {
            return Err(PredictorError::Payload("missing channel".to_owned()));
        };
        if channel != "activeAssetCtx" {
            return Ok(None);
        }
        let data = value
            .get("data")
            .ok_or_else(|| PredictorError::Payload("missing data".to_owned()))?;
        let coin = data
            .get("coin")
            .and_then(Value::as_str)
            .ok_or_else(|| PredictorError::Payload("missing data.coin".to_owned()))?;
        let context = data
            .get("ctx")
            .ok_or_else(|| PredictorError::Payload("missing data.ctx".to_owned()))?;
        let Some(impact_value) = context.get("impactPxs") else {
            return Ok(None);
        };
        if impact_value.is_null() {
            return Ok(None);
        }
        let impact_prices = impact_value.as_array().ok_or_else(|| {
            PredictorError::Payload("data.ctx.impactPxs must be an array or null".to_owned())
        })?;
        if impact_prices.len() != 2 {
            return Err(PredictorError::Payload(
                "data.ctx.impactPxs must contain bid and ask prices".to_owned(),
            ));
        }
        let oracle_px = decimal_field(context, "oraclePx")?.ok_or_else(|| {
            PredictorError::Payload("data.ctx.oraclePx must not be null".to_owned())
        })?;
        let impact_bid_px = decimal_value(&impact_prices[0], "data.ctx.impactPxs[0]")?;
        let impact_ask_px = decimal_value(&impact_prices[1], "data.ctx.impactPxs[1]")?;
        let reported_premium = decimal_field(context, "premium")?;
        Self::new(
            coin,
            observed_at_ms,
            oracle_px,
            impact_bid_px,
            impact_ask_px,
            reported_premium,
        )
        .map(Some)
    }

    /// Calculate the documented premium from the three input prices.
    ///
    /// # Errors
    ///
    /// Returns an arithmetic error if an input exceeds exact decimal range.
    pub fn calculated_premium(&self) -> Result<Decimal, PredictorError> {
        premium_index(self.oracle_px, self.impact_bid_px, self.impact_ask_px)
    }
}

/// Stateful reconstruction of premium inputs from interleaved M3 asset-context
/// and L2-book frames.
///
/// Official WebSocket asset contexts provide the oracle but omit REST's
/// `impactPxs`. The replay state retains the latest oracle for each coin and
/// computes average execution prices from subsequent full-depth book updates.
#[derive(Debug, Default)]
pub struct PremiumReplay {
    latest_oracle: HashMap<String, Decimal>,
}

impl PremiumReplay {
    /// Create an empty replay state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest one raw WebSocket payload in receive order.
    ///
    /// Asset-context messages update the latest oracle and produce a sample
    /// immediately when REST-style impact prices are present. L2 updates use
    /// the latest causal oracle plus the documented funding impact notional:
    /// 20,000 USDC for BTC/ETH and 6,000 USDC for other assets. Unsupported
    /// channels, a book received before its oracle, and insufficient book depth
    /// return `Ok(None)` and are reflected later as incomplete coverage.
    ///
    /// # Errors
    ///
    /// Returns a payload error for malformed subscribed messages and normal
    /// validation/arithmetic errors for invalid prices, sizes, or overflow.
    pub fn ingest(
        &mut self,
        payload: &str,
        observed_at_ms: TimestampMs,
    ) -> Result<Option<PremiumSample>, PredictorError> {
        let value: Value = serde_json::from_str(payload)
            .map_err(|source| PredictorError::Payload(format!("invalid JSON: {source}")))?;
        let channel = value
            .get("channel")
            .and_then(Value::as_str)
            .ok_or_else(|| PredictorError::Payload("missing channel".to_owned()))?;
        match channel {
            "activeAssetCtx" => self.ingest_asset_context(&value, observed_at_ms),
            "l2Book" => self.ingest_book(&value, observed_at_ms),
            _ => Ok(None),
        }
    }

    fn ingest_asset_context(
        &mut self,
        value: &Value,
        observed_at_ms: TimestampMs,
    ) -> Result<Option<PremiumSample>, PredictorError> {
        let data = payload_data(value)?;
        let coin = payload_coin(data)?;
        let context = data
            .get("ctx")
            .ok_or_else(|| PredictorError::Payload("missing data.ctx".to_owned()))?;
        let oracle_px = decimal_field(context, "oraclePx")?.ok_or_else(|| {
            PredictorError::Payload("data.ctx.oraclePx must not be null".to_owned())
        })?;
        if oracle_px <= Decimal::ZERO {
            return Err(PredictorError::NonPositivePrice {
                field: "oracle_px",
                value: oracle_px,
            });
        }
        self.latest_oracle.insert(coin.to_owned(), oracle_px);

        let Some(impact_value) = context.get("impactPxs") else {
            return Ok(None);
        };
        if impact_value.is_null() {
            return Ok(None);
        }
        let impact_prices = impact_value.as_array().ok_or_else(|| {
            PredictorError::Payload("data.ctx.impactPxs must be an array or null".to_owned())
        })?;
        if impact_prices.len() != 2 {
            return Err(PredictorError::Payload(
                "data.ctx.impactPxs must contain bid and ask prices".to_owned(),
            ));
        }
        PremiumSample::new(
            coin,
            observed_at_ms,
            oracle_px,
            decimal_value(&impact_prices[0], "data.ctx.impactPxs[0]")?,
            decimal_value(&impact_prices[1], "data.ctx.impactPxs[1]")?,
            decimal_field(context, "premium")?,
        )
        .map(Some)
    }

    fn ingest_book(
        &self,
        value: &Value,
        observed_at_ms: TimestampMs,
    ) -> Result<Option<PremiumSample>, PredictorError> {
        let data = payload_data(value)?;
        let coin = payload_coin(data)?;
        let Some(oracle_px) = self.latest_oracle.get(coin).copied() else {
            return Ok(None);
        };
        let levels = data
            .get("levels")
            .and_then(Value::as_array)
            .ok_or_else(|| PredictorError::Payload("data.levels must be an array".to_owned()))?;
        if levels.len() != 2 {
            return Err(PredictorError::Payload(
                "data.levels must contain bid and ask sides".to_owned(),
            ));
        }
        let notional = default_impact_notional(coin);
        let Some(impact_bid_px) = impact_execution_price(&levels[0], notional, "bid")? else {
            return Ok(None);
        };
        let Some(impact_ask_px) = impact_execution_price(&levels[1], notional, "ask")? else {
            return Ok(None);
        };
        PremiumSample::new(
            coin,
            observed_at_ms,
            oracle_px,
            impact_bid_px,
            impact_ask_px,
            None,
        )
        .map(Some)
    }
}

/// An official `predictedFundings` observation used only as a benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfficialBenchmark {
    /// Time the benchmark was observed, used to enforce causality.
    pub observed_at_ms: TimestampMs,
    /// Settlement targeted by the official venue prediction.
    pub settlement_time_ms: TimestampMs,
    /// Benchmark rate normalized to Hyperliquid's one-hour interval.
    #[serde(with = "rust_decimal::serde::str")]
    pub hourly_rate: Decimal,
}

impl OfficialBenchmark {
    /// Normalize one typed `predictedFundings` venue entry to an hourly
    /// benchmark while preserving its target settlement.
    ///
    /// # Errors
    ///
    /// Rejects a missing/zero funding interval or decimal overflow.
    pub fn from_venue_prediction(
        observed_at_ms: TimestampMs,
        prediction: &VenuePrediction,
    ) -> Result<Self, PredictorError> {
        let interval_hours = prediction
            .funding_interval_hours
            .ok_or(PredictorError::MissingBenchmarkInterval)?;
        if interval_hours == 0 {
            return Err(PredictorError::ZeroBenchmarkInterval);
        }
        let hourly_rate = prediction
            .funding_rate
            .checked_div(Decimal::from(interval_hours))
            .ok_or(PredictorError::Arithmetic)?;
        Ok(Self {
            observed_at_ms,
            settlement_time_ms: prediction.next_funding_time,
            hourly_rate,
        })
    }
}

/// One UTC-hour prediction request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictionRequest {
    /// Exact perpetual symbol to predict.
    pub coin: String,
    /// Hour-aligned settlement targeted by the prediction.
    pub settlement_time_ms: TimestampMs,
    /// Latest observation time the prediction may consume.
    pub as_of_ms: TimestampMs,
    /// Optional venue prediction retained only for comparison.
    pub official_benchmark: Option<OfficialBenchmark>,
}

impl PredictionRequest {
    /// Create a request for an hour-aligned future settlement.
    ///
    /// # Errors
    ///
    /// Rejects empty coins, non-hour-aligned settlements, and cutoffs outside
    /// the target hour. A benchmark observed after the cutoff is future data.
    pub fn new(
        coin: impl Into<String>,
        settlement_time_ms: TimestampMs,
        as_of_ms: TimestampMs,
        official_benchmark: Option<OfficialBenchmark>,
    ) -> Result<Self, PredictorError> {
        let coin = coin.into();
        if coin.trim().is_empty() {
            return Err(PredictorError::EmptyCoin);
        }
        let settlement = settlement_time_ms.as_i64();
        if settlement.rem_euclid(FUNDING_HOUR_MS) != 0 {
            return Err(PredictorError::UnalignedSettlement(settlement));
        }
        let start = settlement
            .checked_sub(FUNDING_HOUR_MS)
            .ok_or(PredictorError::Arithmetic)?;
        let as_of = as_of_ms.as_i64();
        if as_of < start || as_of >= settlement {
            return Err(PredictorError::CutoffOutsideWindow {
                as_of_ms: as_of,
                start_ms: start,
                end_ms: settlement,
            });
        }
        if let Some(benchmark) = official_benchmark
            && benchmark.observed_at_ms > as_of_ms
        {
            return Err(PredictorError::FutureBenchmark {
                observed_at_ms: benchmark.observed_at_ms.as_i64(),
                as_of_ms: as_of,
            });
        }
        if let Some(benchmark) = official_benchmark
            && benchmark.settlement_time_ms != settlement_time_ms
        {
            return Err(PredictorError::BenchmarkSettlementMismatch {
                benchmark_ms: benchmark.settlement_time_ms.as_i64(),
                requested_ms: settlement,
            });
        }
        Ok(Self {
            coin,
            settlement_time_ms,
            as_of_ms,
            official_benchmark,
        })
    }

    /// Inclusive start of the target UTC funding hour.
    pub const fn window_start_ms(&self) -> TimestampMs {
        TimestampMs::new(
            self.settlement_time_ms
                .as_i64()
                .saturating_sub(FUNDING_HOUR_MS),
        )
    }
}

/// Deterministic coverage/confidence metadata for a partial hour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredictionCoverage {
    /// Distinct five-second slots represented by accepted samples.
    pub samples_used: u32,
    /// Sampling slots expected between the hour start and cutoff.
    pub expected_samples_so_far: u32,
    /// Accepted slots divided by expected slots, capped at one.
    #[serde(with = "rust_decimal::serde::str")]
    pub coverage_ratio: Decimal,
    /// Fraction of the funding hour elapsed at the cutoff.
    #[serde(with = "rust_decimal::serde::str")]
    pub hour_progress_ratio: Decimal,
    /// Conservative completeness score: coverage × hour progress.
    #[serde(with = "rust_decimal::serde::str")]
    pub confidence_ratio: Decimal,
}

/// Explainable next-hour funding estimate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundingPrediction {
    /// Version of the serialized prediction contract.
    pub schema_version: u32,
    /// Exact perpetual symbol predicted.
    pub coin: String,
    /// Hour-aligned settlement targeted by the estimate.
    pub settlement_time_ms: TimestampMs,
    /// Causal cutoff at which the estimate was generated.
    pub generated_at_ms: TimestampMs,
    /// Arithmetic mean of accepted premium slots.
    #[serde(with = "rust_decimal::serde::str")]
    pub average_premium: Decimal,
    /// Estimated one-hour funding rate as a unitless ratio.
    #[serde(with = "rust_decimal::serde::str")]
    pub predicted_hourly_rate: Decimal,
    /// Sampling coverage and deterministic confidence metadata.
    pub coverage: PredictionCoverage,
    /// Official venue prediction retained as a comparison, never as truth or
    /// an input to `predicted_hourly_rate`.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub official_benchmark_hourly_rate: Option<Decimal>,
}

/// A realized hourly settlement joined to its canonical UTC funding hour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealizedSettlement {
    /// Exact perpetual symbol settled.
    pub coin: String,
    /// Canonical hour-aligned settlement time.
    pub settlement_time_ms: TimestampMs,
    /// Realized one-hour funding rate as a unitless ratio.
    pub hourly_rate: Decimal,
}

impl RealizedSettlement {
    /// Construct realized ground truth for an hour-aligned settlement.
    ///
    /// # Errors
    ///
    /// Rejects empty coins and non-hour-aligned settlement identities.
    pub fn new(
        coin: impl Into<String>,
        settlement_time_ms: TimestampMs,
        hourly_rate: Decimal,
    ) -> Result<Self, PredictorError> {
        let coin = coin.into();
        if coin.trim().is_empty() {
            return Err(PredictorError::EmptyCoin);
        }
        if settlement_time_ms.as_i64().rem_euclid(FUNDING_HOUR_MS) != 0 {
            return Err(PredictorError::UnalignedSettlement(
                settlement_time_ms.as_i64(),
            ));
        }
        Ok(Self {
            coin,
            settlement_time_ms,
            hourly_rate,
        })
    }
}

/// Error metrics for one causally valid prediction/settlement join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredictionEvaluation {
    /// Version of the serialized evaluation contract.
    pub schema_version: u32,
    /// Exact perpetual symbol evaluated.
    pub coin: String,
    /// Canonical settlement shared by prediction and ground truth.
    pub settlement_time_ms: TimestampMs,
    /// Predicted one-hour funding rate.
    #[serde(with = "rust_decimal::serde::str")]
    pub predicted_hourly_rate: Decimal,
    /// Realized one-hour funding rate.
    #[serde(with = "rust_decimal::serde::str")]
    pub realized_hourly_rate: Decimal,
    /// `predicted - realized`.
    #[serde(with = "rust_decimal::serde::str")]
    pub signed_error: Decimal,
    /// Absolute value of [`Self::signed_error`].
    #[serde(with = "rust_decimal::serde::str")]
    pub absolute_error: Decimal,
    /// Official benchmark minus realized rate, when a benchmark was supplied.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub official_signed_error: Option<Decimal>,
    /// Absolute official benchmark error, when available.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub official_absolute_error: Option<Decimal>,
}

/// Trailing error statistics ending at one walk-forward settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollingError {
    /// Settlement at the end of this trailing window.
    pub settlement_time_ms: TimestampMs,
    /// Number of evaluations included in the trailing window.
    pub observations: u32,
    /// Arithmetic mean of signed prediction errors.
    #[serde(with = "rust_decimal::serde::str")]
    pub mean_signed_error: Decimal,
    /// Arithmetic mean of absolute prediction errors.
    #[serde(with = "rust_decimal::serde::str")]
    pub mean_absolute_error: Decimal,
}

/// One hour supplied to the deterministic walk-forward evaluator.
#[derive(Debug, Clone)]
pub struct WalkForwardCase {
    /// Causal prediction request for this settlement.
    pub request: PredictionRequest,
    /// Recorded samples that may include observations beyond the request cutoff.
    pub samples: Vec<PremiumSample>,
    /// Realized settlement used as ground truth.
    pub realized: RealizedSettlement,
}

/// Ordered out-of-sample predictions, joins, and trailing errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalkForwardReport {
    /// Version of the serialized walk-forward contract.
    pub schema_version: u32,
    /// Exact perpetual symbol shared by every case.
    pub coin: String,
    /// Maximum observations included in each trailing error statistic.
    pub rolling_window: u32,
    /// Causally generated predictions ordered by settlement.
    pub predictions: Vec<FundingPrediction>,
    /// Prediction-versus-realized joins ordered by settlement.
    pub evaluations: Vec<PredictionEvaluation>,
    /// Trailing error statistics aligned with [`Self::evaluations`].
    pub rolling_errors: Vec<RollingError>,
}

/// Compute the documented premium index.
///
/// # Errors
///
/// Rejects non-positive prices and decimal arithmetic overflow.
pub fn premium_index(
    oracle_px: Decimal,
    impact_bid_px: Decimal,
    impact_ask_px: Decimal,
) -> Result<Decimal, PredictorError> {
    for (field, value) in [
        ("oracle_px", oracle_px),
        ("impact_bid_px", impact_bid_px),
        ("impact_ask_px", impact_ask_px),
    ] {
        if value <= Decimal::ZERO {
            return Err(PredictorError::NonPositivePrice { field, value });
        }
    }
    let bid_premium = impact_bid_px
        .checked_sub(oracle_px)
        .ok_or(PredictorError::Arithmetic)?
        .max(Decimal::ZERO);
    let ask_discount = oracle_px
        .checked_sub(impact_ask_px)
        .ok_or(PredictorError::Arithmetic)?
        .max(Decimal::ZERO);
    bid_premium
        .checked_sub(ask_discount)
        .and_then(|difference| difference.checked_div(oracle_px))
        .ok_or(PredictorError::Arithmetic)
}

/// Apply Hyperliquid's eight-hour interest/clamp formula, convert it to one
/// hour, and enforce the documented ±4% hourly cap.
///
/// # Errors
///
/// Returns an arithmetic error if the decimal operations overflow.
pub fn hourly_funding_from_average_premium(
    average_premium: Decimal,
) -> Result<Decimal, PredictorError> {
    let difference = interest_rate_eight_hour()
        .checked_sub(average_premium)
        .ok_or(PredictorError::Arithmetic)?;
    let bound = clamp_bound_eight_hour();
    let clamped = difference.clamp(-bound, bound);
    let hourly = average_premium
        .checked_add(clamped)
        .and_then(|rate| rate.checked_div(eight()))
        .ok_or(PredictorError::Arithmetic)?;
    let cap = hourly_funding_cap();
    Ok(hourly.clamp(-cap, cap))
}

/// Estimate the target settlement from samples available at the request cutoff.
///
/// Future observations and observations outside the target hour are ignored.
/// Within each UTC-aligned five-second slot the latest received sample wins;
/// conflicting samples with the exact same timestamp are rejected.
///
/// # Errors
///
/// Returns a typed error for missing samples, conflicting receive timestamps,
/// invalid prices, future benchmark leakage, or decimal overflow.
pub fn predict_next_hour(
    request: &PredictionRequest,
    samples: &[PremiumSample],
) -> Result<FundingPrediction, PredictorError> {
    let request = PredictionRequest::new(
        request.coin.clone(),
        request.settlement_time_ms,
        request.as_of_ms,
        request.official_benchmark,
    )?;
    let start = request.window_start_ms().as_i64();
    let cutoff = request.as_of_ms.as_i64();
    let end = request.settlement_time_ms.as_i64();
    let mut slots = BTreeMap::<i64, &PremiumSample>::new();
    for sample in samples.iter().filter(|sample| {
        sample.coin == request.coin
            && sample.observed_at_ms.as_i64() >= start
            && sample.observed_at_ms.as_i64() <= cutoff
            && sample.observed_at_ms.as_i64() < end
    }) {
        validate_sample(sample)?;
        let timestamp = sample.observed_at_ms.as_i64();
        let slot = timestamp.div_euclid(PREMIUM_SAMPLE_INTERVAL_MS);
        match slots.get(&slot) {
            Some(existing) if existing.observed_at_ms == sample.observed_at_ms => {
                if *existing != sample {
                    return Err(PredictorError::ConflictingSampleTimestamp(timestamp));
                }
            }
            Some(existing) if existing.observed_at_ms > sample.observed_at_ms => {}
            _ => {
                slots.insert(slot, sample);
            }
        }
    }
    if slots.is_empty() {
        return Err(PredictorError::NoSamples {
            coin: request.coin.clone(),
            start_ms: start,
            as_of_ms: cutoff,
        });
    }

    let sum = slots.values().try_fold(Decimal::ZERO, |sum, sample| {
        sum.checked_add(sample.calculated_premium()?)
            .ok_or(PredictorError::Arithmetic)
    })?;
    let samples_used = u32::try_from(slots.len()).map_err(|_| PredictorError::Arithmetic)?;
    let average_premium = sum
        .checked_div(Decimal::from(samples_used))
        .ok_or(PredictorError::Arithmetic)?;
    let expected_samples_so_far = u32::try_from(
        cutoff
            .saturating_sub(start)
            .div_euclid(PREMIUM_SAMPLE_INTERVAL_MS)
            .saturating_add(1),
    )
    .unwrap_or(PREMIUM_SAMPLES_PER_HOUR)
    .min(PREMIUM_SAMPLES_PER_HOUR);
    let coverage_ratio = Decimal::from(samples_used)
        .checked_div(Decimal::from(expected_samples_so_far))
        .ok_or(PredictorError::Arithmetic)?
        .min(Decimal::ONE);
    let hour_progress_ratio = Decimal::from(cutoff.saturating_sub(start))
        .checked_div(Decimal::from(FUNDING_HOUR_MS))
        .ok_or(PredictorError::Arithmetic)?;
    let confidence_ratio = coverage_ratio
        .checked_mul(hour_progress_ratio)
        .ok_or(PredictorError::Arithmetic)?;

    Ok(FundingPrediction {
        schema_version: PREDICTION_SCHEMA_VERSION,
        coin: request.coin.clone(),
        settlement_time_ms: request.settlement_time_ms,
        generated_at_ms: request.as_of_ms,
        average_premium,
        predicted_hourly_rate: hourly_funding_from_average_premium(average_premium)?,
        coverage: PredictionCoverage {
            samples_used,
            expected_samples_so_far,
            coverage_ratio,
            hour_progress_ratio,
            confidence_ratio,
        },
        official_benchmark_hourly_rate: request
            .official_benchmark
            .map(|benchmark| benchmark.hourly_rate),
    })
}

/// Join one prediction to realized funding without future-data leakage.
///
/// # Errors
///
/// Rejects identity mismatches and any prediction generated at or after its
/// target settlement.
pub fn evaluate_prediction(
    prediction: &FundingPrediction,
    realized: &RealizedSettlement,
) -> Result<PredictionEvaluation, PredictorError> {
    RealizedSettlement::new(
        realized.coin.clone(),
        realized.settlement_time_ms,
        realized.hourly_rate,
    )?;
    if prediction.coin != realized.coin
        || prediction.settlement_time_ms != realized.settlement_time_ms
    {
        return Err(PredictorError::SettlementMismatch);
    }
    if prediction.generated_at_ms >= prediction.settlement_time_ms {
        return Err(PredictorError::PredictionAfterSettlement {
            generated_at_ms: prediction.generated_at_ms.as_i64(),
            settlement_time_ms: prediction.settlement_time_ms.as_i64(),
        });
    }
    let signed_error = prediction
        .predicted_hourly_rate
        .checked_sub(realized.hourly_rate)
        .ok_or(PredictorError::Arithmetic)?;
    let official_signed_error = prediction
        .official_benchmark_hourly_rate
        .map(|rate| {
            rate.checked_sub(realized.hourly_rate)
                .ok_or(PredictorError::Arithmetic)
        })
        .transpose()?;
    Ok(PredictionEvaluation {
        schema_version: PREDICTION_SCHEMA_VERSION,
        coin: prediction.coin.clone(),
        settlement_time_ms: prediction.settlement_time_ms,
        predicted_hourly_rate: prediction.predicted_hourly_rate,
        realized_hourly_rate: realized.hourly_rate,
        signed_error,
        absolute_error: signed_error.abs(),
        official_signed_error,
        official_absolute_error: official_signed_error.map(|error| error.abs()),
    })
}

/// Run deterministic walk-forward cases and compute trailing mean signed and
/// absolute error over `rolling_window` settlements.
///
/// Cases are sorted by settlement. Every case is independently cut off at its
/// `as_of_ms`; later samples cannot affect earlier predictions.
///
/// # Errors
///
/// Rejects empty or mixed-coin cases, duplicate settlement identities, an
/// invalid prediction/realized join, and decimal overflow.
pub fn walk_forward(
    cases: &[WalkForwardCase],
    rolling_window: NonZeroUsize,
) -> Result<WalkForwardReport, PredictorError> {
    let Some(first) = cases.first() else {
        return Err(PredictorError::EmptyBacktest);
    };
    let coin = first.request.coin.clone();
    if cases
        .iter()
        .any(|case| case.request.coin != coin || case.realized.coin != coin)
    {
        return Err(PredictorError::MixedBacktestCoins);
    }
    let mut ordered: Vec<_> = cases.iter().collect();
    ordered.sort_by_key(|case| case.request.settlement_time_ms);
    if ordered
        .windows(2)
        .any(|pair| pair[0].request.settlement_time_ms == pair[1].request.settlement_time_ms)
    {
        return Err(PredictorError::DuplicateBacktestSettlement);
    }

    let mut predictions = Vec::with_capacity(ordered.len());
    let mut evaluations = Vec::with_capacity(ordered.len());
    for case in ordered {
        let prediction = predict_next_hour(&case.request, &case.samples)?;
        let evaluation = evaluate_prediction(&prediction, &case.realized)?;
        predictions.push(prediction);
        evaluations.push(evaluation);
    }
    let mut rolling_errors = Vec::with_capacity(evaluations.len());
    for end in 0..evaluations.len() {
        let start = end.saturating_add(1).saturating_sub(rolling_window.get());
        let window = &evaluations[start..=end];
        let (signed_sum, absolute_sum) = window.iter().try_fold(
            (Decimal::ZERO, Decimal::ZERO),
            |(signed, absolute), evaluation| {
                Ok::<_, PredictorError>((
                    signed
                        .checked_add(evaluation.signed_error)
                        .ok_or(PredictorError::Arithmetic)?,
                    absolute
                        .checked_add(evaluation.absolute_error)
                        .ok_or(PredictorError::Arithmetic)?,
                ))
            },
        )?;
        let observations = u32::try_from(window.len()).map_err(|_| PredictorError::Arithmetic)?;
        let denominator = Decimal::from(observations);
        rolling_errors.push(RollingError {
            settlement_time_ms: evaluations[end].settlement_time_ms,
            observations,
            mean_signed_error: signed_sum
                .checked_div(denominator)
                .ok_or(PredictorError::Arithmetic)?,
            mean_absolute_error: absolute_sum
                .checked_div(denominator)
                .ok_or(PredictorError::Arithmetic)?,
        });
    }
    Ok(WalkForwardReport {
        schema_version: PREDICTION_SCHEMA_VERSION,
        coin,
        rolling_window: u32::try_from(rolling_window.get()).unwrap_or(u32::MAX),
        predictions,
        evaluations,
        rolling_errors,
    })
}

fn validate_sample(sample: &PremiumSample) -> Result<(), PredictorError> {
    PremiumSample::new(
        sample.coin.clone(),
        sample.observed_at_ms,
        sample.oracle_px,
        sample.impact_bid_px,
        sample.impact_ask_px,
        sample.reported_premium,
    )?;
    Ok(())
}

fn payload_data(value: &Value) -> Result<&Value, PredictorError> {
    value
        .get("data")
        .ok_or_else(|| PredictorError::Payload("missing data".to_owned()))
}

fn payload_coin(data: &Value) -> Result<&str, PredictorError> {
    data.get("coin")
        .and_then(Value::as_str)
        .ok_or_else(|| PredictorError::Payload("missing data.coin".to_owned()))
}

fn impact_execution_price(
    levels: &Value,
    target_notional: Decimal,
    side_name: &'static str,
) -> Result<Option<Decimal>, PredictorError> {
    let levels = levels.as_array().ok_or_else(|| {
        PredictorError::Payload(format!("data.levels {side_name} side must be an array"))
    })?;
    let mut remaining = target_notional;
    let mut base_filled = Decimal::ZERO;
    for level in levels {
        let price = decimal_field(level, "px")?.ok_or_else(|| {
            PredictorError::Payload(format!("data.levels {side_name} price must not be null"))
        })?;
        let size = decimal_field(level, "sz")?.ok_or_else(|| {
            PredictorError::Payload(format!("data.levels {side_name} size must not be null"))
        })?;
        if price <= Decimal::ZERO {
            return Err(PredictorError::NonPositivePrice {
                field: "book_px",
                value: price,
            });
        }
        if size <= Decimal::ZERO {
            return Err(PredictorError::NonPositiveBookSize {
                side: side_name,
                value: size,
            });
        }
        let level_notional = price.checked_mul(size).ok_or(PredictorError::Arithmetic)?;
        let quote_filled = level_notional.min(remaining);
        base_filled = base_filled
            .checked_add(
                quote_filled
                    .checked_div(price)
                    .ok_or(PredictorError::Arithmetic)?,
            )
            .ok_or(PredictorError::Arithmetic)?;
        remaining = remaining
            .checked_sub(quote_filled)
            .ok_or(PredictorError::Arithmetic)?;
        if remaining == Decimal::ZERO {
            return target_notional
                .checked_div(base_filled)
                .map(Some)
                .ok_or(PredictorError::Arithmetic);
        }
    }
    Ok(None)
}

fn decimal_field(value: &Value, field: &'static str) -> Result<Option<Decimal>, PredictorError> {
    let Some(raw) = value.get(field) else {
        return Err(PredictorError::Payload(format!(
            "missing decimal field {field}"
        )));
    };
    if raw.is_null() {
        return Ok(None);
    }
    decimal_value(raw, field).map(Some)
}

fn decimal_value(value: &Value, field: &'static str) -> Result<Decimal, PredictorError> {
    let text = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    Decimal::from_str(&text)
        .map_err(|source| PredictorError::Payload(format!("{field} is not a decimal: {source}")))
}

/// Validation, causality, or exact-arithmetic failure from prediction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PredictorError {
    /// A perpetual symbol was empty or whitespace-only.
    EmptyCoin,
    /// A price input was zero or negative.
    NonPositivePrice {
        /// Name of the rejected price field.
        field: &'static str,
        /// Rejected price value.
        value: Decimal,
    },
    /// An order-book level contained a zero or negative size.
    NonPositiveBookSize {
        /// Book side containing the invalid level.
        side: &'static str,
        /// Rejected size value.
        value: Decimal,
    },
    /// A recorder payload was malformed or lacked a required field.
    Payload(String),
    /// A settlement timestamp was not aligned to a UTC hour.
    UnalignedSettlement(i64),
    /// The prediction cutoff fell outside its target funding hour.
    CutoffOutsideWindow {
        /// Requested prediction cutoff.
        as_of_ms: i64,
        /// Inclusive start of the target funding hour.
        start_ms: i64,
        /// Exclusive end and settlement of the target funding hour.
        end_ms: i64,
    },
    /// An official benchmark was observed after the prediction cutoff.
    FutureBenchmark {
        /// Time at which the benchmark was observed.
        observed_at_ms: i64,
        /// Latest observation time permitted by the request.
        as_of_ms: i64,
    },
    /// An official benchmark targeted a different settlement.
    BenchmarkSettlementMismatch {
        /// Settlement targeted by the benchmark.
        benchmark_ms: i64,
        /// Settlement targeted by the prediction request.
        requested_ms: i64,
    },
    /// An official venue prediction omitted its settlement interval.
    MissingBenchmarkInterval,
    /// An official venue prediction declared a zero-hour interval.
    ZeroBenchmarkInterval,
    /// No usable observations existed for the requested coin and window.
    NoSamples {
        /// Requested perpetual symbol.
        coin: String,
        /// Inclusive start of the target funding hour.
        start_ms: i64,
        /// Latest observation time permitted by the request.
        as_of_ms: i64,
    },
    /// Two distinct samples shared the same receive timestamp.
    ConflictingSampleTimestamp(i64),
    /// Prediction and realized settlement identities differed.
    SettlementMismatch,
    /// A prediction was generated at or after its target settlement.
    PredictionAfterSettlement {
        /// Prediction generation time.
        generated_at_ms: i64,
        /// Settlement targeted by the prediction.
        settlement_time_ms: i64,
    },
    /// A walk-forward evaluation contained no cases.
    EmptyBacktest,
    /// Walk-forward cases did not share one exact perpetual symbol.
    MixedBacktestCoins,
    /// Multiple walk-forward cases targeted the same settlement.
    DuplicateBacktestSettlement,
    /// An exact decimal or integer arithmetic operation overflowed.
    Arithmetic,
}

impl fmt::Display for PredictorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCoin => formatter.write_str("predictor coin must not be empty"),
            Self::NonPositivePrice { field, value } => {
                write!(formatter, "predictor {field} must be positive, got {value}")
            }
            Self::NonPositiveBookSize { side, value } => {
                write!(
                    formatter,
                    "predictor {side} book size must be positive, got {value}"
                )
            }
            Self::Payload(message) => {
                write!(formatter, "market-data payload is invalid: {message}")
            }
            Self::UnalignedSettlement(value) => write!(
                formatter,
                "settlement {value}ms must be aligned to a UTC hour"
            ),
            Self::CutoffOutsideWindow {
                as_of_ms,
                start_ms,
                end_ms,
            } => write!(
                formatter,
                "prediction cutoff {as_of_ms}ms is outside [{start_ms}, {end_ms})"
            ),
            Self::FutureBenchmark {
                observed_at_ms,
                as_of_ms,
            } => write!(
                formatter,
                "official benchmark observed at {observed_at_ms}ms is later than prediction cutoff {as_of_ms}ms"
            ),
            Self::BenchmarkSettlementMismatch {
                benchmark_ms,
                requested_ms,
            } => write!(
                formatter,
                "official benchmark targets {benchmark_ms}ms, not requested settlement {requested_ms}ms"
            ),
            Self::MissingBenchmarkInterval => {
                formatter.write_str("official benchmark funding interval is missing")
            }
            Self::ZeroBenchmarkInterval => {
                formatter.write_str("official benchmark funding interval must be positive")
            }
            Self::NoSamples {
                coin,
                start_ms,
                as_of_ms,
            } => write!(
                formatter,
                "no premium samples for {coin:?} in [{start_ms}, {as_of_ms}]"
            ),
            Self::ConflictingSampleTimestamp(value) => write!(
                formatter,
                "premium samples at {value}ms conflict; receive-time identity must be deterministic"
            ),
            Self::SettlementMismatch => {
                formatter.write_str("prediction and realized settlement identities do not match")
            }
            Self::PredictionAfterSettlement {
                generated_at_ms,
                settlement_time_ms,
            } => write!(
                formatter,
                "prediction generated at {generated_at_ms}ms is not before settlement {settlement_time_ms}ms"
            ),
            Self::EmptyBacktest => {
                formatter.write_str("walk-forward backtest requires at least one case")
            }
            Self::MixedBacktestCoins => formatter.write_str("walk-forward cases must use one coin"),
            Self::DuplicateBacktestSettlement => {
                formatter.write_str("walk-forward cases contain a duplicate settlement")
            }
            Self::Arithmetic => {
                formatter.write_str("predictor decimal arithmetic exceeded exact range")
            }
        }
    }
}

impl Error for PredictorError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(value: &str) -> Decimal {
        value.parse().expect("valid test decimal")
    }

    fn sample(time: i64, bid: &str, ask: &str) -> PremiumSample {
        PremiumSample::new(
            "BTC",
            TimestampMs::new(time),
            dec("100"),
            dec(bid),
            dec(ask),
            None,
        )
        .expect("valid sample")
    }

    #[test]
    fn documented_formula_handles_bid_premium_ask_discount_and_clamp() {
        assert_eq!(
            premium_index(dec("100"), dec("101"), dec("102")).unwrap(),
            dec("0.01")
        );
        assert_eq!(
            premium_index(dec("100"), dec("99"), dec("98")).unwrap(),
            dec("-0.02")
        );
        assert_eq!(
            hourly_funding_from_average_premium(dec("0.01")).unwrap(),
            dec("0.0011875")
        );
        assert_eq!(
            hourly_funding_from_average_premium(Decimal::ZERO).unwrap(),
            dec("0.0000125")
        );

        let benchmark = OfficialBenchmark::from_venue_prediction(
            TimestampMs::new(1),
            &VenuePrediction {
                funding_rate: dec("0.008"),
                next_funding_time: TimestampMs::new(FUNDING_HOUR_MS),
                funding_interval_hours: Some(8),
            },
        )
        .expect("declared venue interval normalizes");
        assert_eq!(benchmark.hourly_rate, dec("0.001"));
    }

    #[test]
    fn payload_parser_preserves_reported_premium_and_rejects_bad_impact_shape() {
        let payload = r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"oraclePx":"100","impactPxs":["101","102"],"premium":"0.01"}}}"#;
        let parsed = PremiumSample::from_asset_context_payload(payload, TimestampMs::new(1))
            .expect("payload parses")
            .expect("sample available");
        assert_eq!(parsed.reported_premium, Some(dec("0.01")));
        assert_eq!(parsed.calculated_premium().unwrap(), dec("0.01"));

        let invalid = payload.replace("[\"101\",\"102\"]", "[\"101\"]");
        assert!(matches!(
            PremiumSample::from_asset_context_payload(&invalid, TimestampMs::new(1)),
            Err(PredictorError::Payload(_))
        ));
    }

    #[test]
    fn replay_reconstructs_impact_prices_from_official_websocket_shapes() {
        let mut replay = PremiumReplay::new();
        let context = r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"oraclePx":"100","funding":"0.0","openInterest":"1"}}}"#;
        assert_eq!(
            replay
                .ingest(context, TimestampMs::new(1_000))
                .expect("official context parses"),
            None
        );
        let book = r#"{"channel":"l2Book","data":{"coin":"BTC","time":1001,"levels":[[{"px":"101","sz":"200","n":1}],[{"px":"102","sz":"200","n":1}]]}}"#;
        let sample = replay
            .ingest(book, TimestampMs::new(1_001))
            .expect("book parses")
            .expect("documented impact notional is available");
        assert_eq!(sample.oracle_px, dec("100"));
        assert_eq!(sample.impact_bid_px, dec("101"));
        assert_eq!(sample.impact_ask_px, dec("102"));
        assert_eq!(sample.calculated_premium().unwrap(), dec("0.01"));

        let shallow = r#"{"channel":"l2Book","data":{"coin":"BTC","levels":[[{"px":"101","sz":"1","n":1}],[{"px":"102","sz":"1","n":1}]]}}"#;
        assert_eq!(
            replay
                .ingest(shallow, TimestampMs::new(1_002))
                .expect("shallow book remains valid"),
            None
        );
    }

    #[test]
    fn partial_prediction_collapses_slots_and_ignores_future_samples() {
        let request = PredictionRequest::new(
            "BTC",
            TimestampMs::new(FUNDING_HOUR_MS),
            TimestampMs::new(10_000),
            None,
        )
        .unwrap();
        let samples = vec![
            sample(0, "101", "102"),
            sample(1_000, "101", "102"),
            sample(5_000, "99", "98"),
            sample(15_000, "150", "150"),
        ];
        let prediction = predict_next_hour(&request, &samples).unwrap();
        assert_eq!(prediction.coverage.samples_used, 2);
        assert_eq!(prediction.coverage.expected_samples_so_far, 3);
        assert_eq!(prediction.average_premium, dec("-0.005"));

        let mut changed_future = samples;
        changed_future[3] = sample(15_000, "1000", "1000");
        assert_eq!(
            predict_next_hour(&request, &changed_future).unwrap(),
            prediction
        );
    }

    #[test]
    fn conflicting_same_timestamp_and_future_benchmark_fail_closed() {
        let request = PredictionRequest::new(
            "BTC",
            TimestampMs::new(FUNDING_HOUR_MS),
            TimestampMs::new(10_000),
            None,
        )
        .unwrap();
        assert!(matches!(
            predict_next_hour(
                &request,
                &[sample(1_000, "101", "102"), sample(1_000, "102", "102")]
            ),
            Err(PredictorError::ConflictingSampleTimestamp(1_000))
        ));
        assert!(matches!(
            PredictionRequest::new(
                "BTC",
                TimestampMs::new(FUNDING_HOUR_MS),
                TimestampMs::new(10_000),
                Some(OfficialBenchmark {
                    observed_at_ms: TimestampMs::new(10_001),
                    settlement_time_ms: TimestampMs::new(FUNDING_HOUR_MS),
                    hourly_rate: Decimal::ZERO,
                }),
            ),
            Err(PredictorError::FutureBenchmark { .. })
        ));
    }

    #[test]
    fn evaluation_and_walk_forward_report_baseline_and_benchmark_errors() {
        let make_case = |hour: i64, bid: &str, realized: &str| {
            let settlement = hour * FUNDING_HOUR_MS;
            WalkForwardCase {
                request: PredictionRequest::new(
                    "BTC",
                    TimestampMs::new(settlement),
                    TimestampMs::new(settlement - 1),
                    Some(OfficialBenchmark {
                        observed_at_ms: TimestampMs::new(settlement - 2),
                        settlement_time_ms: TimestampMs::new(settlement),
                        hourly_rate: dec("0.001"),
                    }),
                )
                .unwrap(),
                samples: vec![sample(settlement - 5_000, bid, "102")],
                realized: RealizedSettlement::new(
                    "BTC",
                    TimestampMs::new(settlement),
                    dec(realized),
                )
                .unwrap(),
            }
        };
        let report = walk_forward(
            &[make_case(1, "101", "0.001"), make_case(2, "102", "0.002")],
            NonZeroUsize::new(2).unwrap(),
        )
        .unwrap();
        assert_eq!(report.evaluations.len(), 2);
        assert_eq!(report.rolling_errors[0].observations, 1);
        assert_eq!(report.rolling_errors[1].observations, 2);
        assert_eq!(report.rolling_errors[1].mean_signed_error, dec("0.0003125"));
        assert_eq!(
            report.rolling_errors[1].mean_absolute_error,
            dec("0.0003125")
        );
        assert_eq!(
            report.evaluations[0].official_absolute_error,
            Some(Decimal::ZERO)
        );
    }
}
