//! Serde models for the Hyperliquid `info`-endpoint responses that hypercarry
//! consumes. All are **inbound** response types, so they derive `Deserialize`
//! only. Monetary and rate values are [`Decimal`], never `f64`.
//!
//! Endpoints modeled (`POST https://api.hyperliquid.xyz/info`):
//! - `fundingHistory`  -> [`FundingHistory`]
//! - `metaAndAssetCtxs` -> [`MetaAndAssetCtxs`]
//! - `predictedFundings` -> [`PredictedFundings`]

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Unix timestamp measured in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct TimestampMs(i64);

impl TimestampMs {
    /// Wrap a unix-millisecond value.
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Read the wrapped unix-millisecond value.
    pub const fn as_i64(self) -> i64 {
        self.0
    }
}

/// Full response of a `fundingHistory` query: settled hourly funding points.
pub type FundingHistory = Vec<FundingHistoryEntry>;

/// One settled funding observation.
///
/// Request body: `{"type":"fundingHistory","coin":"BTC","startTime":<ms>}`.
/// Hyperliquid settles funding hourly, so each entry represents one hour.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FundingHistoryEntry {
    /// Perpetual market symbol.
    pub coin: String,
    /// Settled hourly funding rate as a unitless ratio.
    #[serde(with = "rust_decimal::serde::str")]
    pub funding_rate: Decimal,
    /// Premium component used to calculate this settlement.
    #[serde(with = "rust_decimal::serde::str")]
    pub premium: Decimal,
    /// Settlement time, unix milliseconds.
    pub time: TimestampMs,
}

/// Response of `metaAndAssetCtxs`: a 2-tuple of perp metadata and the parallel
/// per-asset context array (same order as `meta.universe`).
#[derive(Debug, Clone, Deserialize)]
pub struct MetaAndAssetCtxs(pub Meta, pub Vec<AssetCtx>);

/// Perp universe metadata.
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    /// Ordered perpetual-market definitions corresponding to the context list.
    pub universe: Vec<AssetMeta>,
}

/// One asset's static metadata within the universe.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetMeta {
    /// Perpetual market symbol.
    pub name: String,
    /// Supported decimal places for order size.
    pub sz_decimals: u32,
    /// Maximum venue leverage for the market.
    pub max_leverage: u32,
    /// Whether the market permits only isolated-margin positions.
    #[serde(default)]
    pub only_isolated: bool,
    /// Set on assets that have been delisted. Their live [`AssetCtx`] carries
    /// `null` mid/premium/impact prices.
    #[serde(default)]
    pub is_delisted: bool,
}

/// One asset's live context (mark/oracle/funding/OI, etc.), positionally aligned
/// with [`Meta::universe`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetCtx {
    /// Current hourly funding rate.
    #[serde(with = "rust_decimal::serde::str")]
    pub funding: Decimal,
    /// Current aggregate open interest, in base-asset units.
    #[serde(with = "rust_decimal::serde::str")]
    pub open_interest: Decimal,
    /// Current oracle price.
    #[serde(with = "rust_decimal::serde::str")]
    pub oracle_px: Decimal,
    /// Current perpetual mark price.
    #[serde(with = "rust_decimal::serde::str")]
    pub mark_px: Decimal,
    /// Current midpoint, absent when the market has no live book.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub mid_px: Option<Decimal>,
    /// Mark-vs-oracle premium; the running input to the funding predictor (M4).
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub premium: Option<Decimal>,
    /// `[bid_impact_px, ask_impact_px]` when present; empty when the API sends
    /// `null` (delisted assets have no live book).
    #[serde(default, deserialize_with = "de::dec_vec")]
    pub impact_pxs: Vec<Decimal>,
    /// Previous day's reference price, when supplied by the venue.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub prev_day_px: Option<Decimal>,
    /// Current day's notional volume, when supplied by the venue.
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub day_ntl_vlm: Option<Decimal>,
}

/// Full response of `predictedFundings`: one entry per coin. Only the first perp
/// dex is supported by this endpoint.
pub type PredictedFundings = Vec<CoinPredictedFundings>;

/// Predicted funding for a single coin across venues: `(coin, per-venue list)`.
#[derive(Debug, Clone, Deserialize)]
pub struct CoinPredictedFundings(pub String, pub Vec<VenuePredictionEntry>);

/// A `(venue_name, prediction)` pair. The prediction is optional to tolerate any
/// null venue slot the API may emit.
#[derive(Debug, Clone, Deserialize)]
pub struct VenuePredictionEntry(pub String, pub Option<VenuePrediction>);

/// One venue's predicted funding. Shape verified against a live API response
/// (2026-07-08): venues observed were `BinPerp`/`HlPerp`/`BybitPerp`, with
/// `fundingIntervalHours` omitted for some venues and whole predictions `null`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VenuePrediction {
    /// Venue funding rate for its native settlement interval.
    #[serde(with = "rust_decimal::serde::str")]
    pub funding_rate: Decimal,
    /// Next settlement time for this venue, unix milliseconds.
    pub next_funding_time: TimestampMs,
    /// Settlement interval in hours (e.g. 1 for Hyperliquid, 8 for Binance).
    /// Needed to normalize rates before comparing across venues (M2 `spread`).
    #[serde(default)]
    pub funding_interval_hours: Option<u32>,
}

mod de {
    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer};
    use std::str::FromStr;

    /// Deserialize a JSON array of decimal *strings* into `Vec<Decimal>`.
    ///
    /// The live API sends an explicit `null` (not a missing key) for assets
    /// without a live order book — e.g. delisted perps — so `null` maps to an
    /// empty vec rather than an error.
    pub fn dec_vec<'de, D>(d: D) -> Result<Vec<Decimal>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: Option<Vec<String>> = Option::deserialize(d)?;
        raw.unwrap_or_default()
            .into_iter()
            .map(|s| Decimal::from_str(&s).map_err(serde::de::Error::custom))
            .collect()
    }
}

/// Static metadata and live context for one positionally joined market.
#[derive(Debug, Clone)]
pub struct AssetSnapshot {
    /// Static market metadata.
    pub meta: AssetMeta,
    /// Live market context.
    pub ctx: AssetCtx,
}

/// Length mismatch while joining parallel metadata and context arrays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinError {
    /// Number of entries in the metadata universe.
    pub universe_len: usize,
    /// Number of entries in the live-context array.
    pub ctxs_len: usize,
}

impl MetaAndAssetCtxs {
    /// Join the positionally aligned metadata and live-context arrays.
    ///
    /// Hyperliquid correlates `meta.universe[i]` with `ctxs[i]`. A length check
    /// must happen before `zip`, because `zip` silently truncates to the shorter
    /// iterator.
    ///
    /// # Errors
    ///
    /// Returns [`JoinError`] when the universe and context arrays have
    /// different lengths.
    pub fn into_snapshots(self) -> Result<Vec<AssetSnapshot>, JoinError> {
        let Self(meta, ctxs) = self;
        let universe_len = meta.universe.len();
        let ctxs_len = ctxs.len();

        if universe_len != ctxs_len {
            return Err(JoinError {
                universe_len,
                ctxs_len,
            });
        }

        let snapshots = meta
            .universe
            .into_iter()
            .zip(ctxs)
            .map(|(meta, ctx)| AssetSnapshot { meta, ctx })
            .collect();

        Ok(snapshots)
    }
}
