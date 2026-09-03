//! Settled-funding Parquet schema, partition layout, and row provenance.

use chrono::{DateTime, Utc};
use hypercarry_core::{
    info::Network,
    types::{FundingHistoryEntry, TimestampMs},
};
use parquet::schema::{parser::parse_message_type, types::Type};
use rust_decimal::Decimal;
use std::{
    cmp::Ordering,
    error::Error,
    fmt,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
};

/// Stable schema version embedded in every row and partition path.
pub const SCHEMA_VERSION: u32 = 1;

/// Fixed scale used by both Parquet decimal columns.
pub const DECIMAL_SCALE: u32 = 18;

/// Root directory name for the settled-funding dataset.
pub const DATASET_NAME: &str = "settled_funding";

/// Parquet message schema for settled-funding version 1.
///
/// Rates use `DECIMAL(38, 18)`: every value accepted by this contract is
/// represented exactly, and higher-scale values are rejected rather than
/// rounded. Timestamps are UTC-adjusted Unix milliseconds.
pub const PARQUET_SCHEMA: &str = r"
message settled_funding_v1 {
  REQUIRED INT32 schema_version;
  REQUIRED BYTE_ARRAY software_version (STRING);
  REQUIRED BYTE_ARRAY network (STRING);
  REQUIRED BYTE_ARRAY venue (STRING);
  REQUIRED BYTE_ARRAY coin (STRING);
  REQUIRED INT64 settlement_time_ms (TIMESTAMP(MILLIS,true));
  REQUIRED FIXED_LEN_BYTE_ARRAY(16) funding_rate (DECIMAL(38,18));
  REQUIRED FIXED_LEN_BYTE_ARRAY(16) premium (DECIMAL(38,18));
  REQUIRED BYTE_ARRAY source_endpoint_class (STRING);
  REQUIRED INT64 ingestion_time_ms (TIMESTAMP(MILLIS,true));
  REQUIRED INT64 request_start_time_ms (TIMESTAMP(MILLIS,true));
  REQUIRED INT64 request_end_time_ms (TIMESTAMP(MILLIS,true));
}
";

/// Parse and validate the frozen Parquet schema.
///
/// # Errors
///
/// Returns the upstream Parquet schema error if the compile-time contract is
/// invalid. Callers may cache the returned type.
pub fn parquet_schema() -> parquet::errors::Result<Arc<Type>> {
    parse_message_type(PARQUET_SCHEMA).map(Arc::new)
}

/// Stable endpoint class stored as ingestion provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceEndpointClass {
    /// One of Hyperliquid's official public network endpoints.
    Official,
    /// An explicitly configured development or test endpoint.
    Development,
}

impl SourceEndpointClass {
    /// Stable lowercase value written to the dataset.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::Development => "development",
        }
    }
}

/// Inclusive source request window, measured in Unix milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestWindow {
    start: TimestampMs,
    end: TimestampMs,
}

impl RequestWindow {
    /// Construct an inclusive request window.
    ///
    /// # Errors
    ///
    /// Returns [`StorageContractError::ReversedRequestWindow`] when `start` is
    /// later than `end`.
    pub const fn new(start: TimestampMs, end: TimestampMs) -> Result<Self, StorageContractError> {
        if start.as_i64() > end.as_i64() {
            return Err(StorageContractError::ReversedRequestWindow {
                start_ms: start.as_i64(),
                end_ms: end.as_i64(),
            });
        }
        Ok(Self { start, end })
    }

    /// Inclusive lower bound in Unix milliseconds.
    pub const fn start(self) -> TimestampMs {
        self.start
    }

    /// Inclusive upper bound in Unix milliseconds.
    pub const fn end(self) -> TimestampMs {
        self.end
    }

    const fn contains(self, timestamp: TimestampMs) -> bool {
        self.start.as_i64() <= timestamp.as_i64() && timestamp.as_i64() <= self.end.as_i64()
    }
}

/// Reproducibility metadata copied onto every settled-funding row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestionProvenance {
    /// Whether the request used an official or development endpoint.
    pub source_endpoint_class: SourceEndpointClass,
    /// Time the response was normalized for storage.
    pub ingestion_time: TimestampMs,
    /// Inclusive request bounds that produced the observation.
    pub request_window: RequestWindow,
    /// Hypercarry package version that produced the row.
    pub software_version: String,
}

impl IngestionProvenance {
    /// Create provenance using the current workspace package version.
    pub fn for_current_build(
        source_endpoint_class: SourceEndpointClass,
        ingestion_time: TimestampMs,
        request_window: RequestWindow,
    ) -> Self {
        Self {
            source_endpoint_class,
            ingestion_time,
            request_window,
            software_version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// Deterministic identity used for ordering and deduplication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledFundingIdentity {
    /// Hyperliquid deployment; mainnet and testnet never share an identity.
    pub network: Network,
    /// Funding venue, currently `hyperliquid` for `fundingHistory`.
    pub venue: String,
    /// Exact market coin symbol.
    pub coin: String,
    /// Funding settlement time in Unix milliseconds.
    pub settlement_time: TimestampMs,
}

impl Ord for SettledFundingIdentity {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            self.network.as_str(),
            self.venue.as_str(),
            self.coin.as_str(),
            self.settlement_time.as_i64(),
        )
            .cmp(&(
                other.network.as_str(),
                other.venue.as_str(),
                other.coin.as_str(),
                other.settlement_time.as_i64(),
            ))
    }
}

impl PartialOrd for SettledFundingIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for SettledFundingIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.network.as_str().hash(state);
        self.venue.hash(state);
        self.coin.hash(state);
        self.settlement_time.as_i64().hash(state);
    }
}

/// One validated row in the settled-funding v1 dataset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledFundingRecord {
    /// Frozen schema version for this row.
    pub schema_version: u32,
    /// Deterministic row identity.
    pub identity: SettledFundingIdentity,
    /// Settled funding rate.
    pub funding_rate: Decimal,
    /// Premium used by the source settlement calculation.
    pub premium: Decimal,
    /// Reproducibility metadata.
    pub provenance: IngestionProvenance,
}

impl SettledFundingRecord {
    /// Normalize an API funding entry into the v1 storage contract.
    ///
    /// # Errors
    ///
    /// Rejects empty identity fields, values that cannot be represented by the
    /// frozen decimal scale, and observations outside their source window.
    pub fn from_history_entry(
        network: Network,
        venue: impl Into<String>,
        entry: FundingHistoryEntry,
        provenance: IngestionProvenance,
    ) -> Result<Self, StorageContractError> {
        let venue = venue.into();
        validate_identity_component("venue", &venue)?;
        validate_identity_component("coin", &entry.coin)?;
        validate_decimal_scale("funding_rate", entry.funding_rate)?;
        validate_decimal_scale("premium", entry.premium)?;
        if !provenance.request_window.contains(entry.time) {
            return Err(StorageContractError::ObservationOutsideRequestWindow {
                settlement_time_ms: entry.time.as_i64(),
                start_ms: provenance.request_window.start.as_i64(),
                end_ms: provenance.request_window.end.as_i64(),
            });
        }

        Ok(Self {
            schema_version: SCHEMA_VERSION,
            identity: SettledFundingIdentity {
                network,
                venue,
                coin: entry.coin,
                settlement_time: entry.time,
            },
            funding_rate: entry.funding_rate,
            premium: entry.premium,
            provenance,
        })
    }

    /// Return the deterministic Hive-style directory for this record.
    ///
    /// Layout:
    /// `settled_funding/schema_version=1/network=.../venue=.../coin=.../settlement_date_utc=YYYY-MM-DD`
    ///
    /// Identity values are percent-encoded so exchange-provided symbols cannot
    /// escape or reshape the intended partition directory.
    ///
    /// # Errors
    ///
    /// Returns [`StorageContractError::TimestampOutOfRange`] if the settlement
    /// timestamp cannot be represented as a calendar date.
    pub fn partition_directory(&self) -> Result<PathBuf, StorageContractError> {
        let date = DateTime::<Utc>::from_timestamp_millis(self.identity.settlement_time.as_i64())
            .ok_or(StorageContractError::TimestampOutOfRange {
                timestamp_ms: self.identity.settlement_time.as_i64(),
            })?
            .date_naive();

        Ok(PathBuf::from(DATASET_NAME)
            .join(format!("schema_version={SCHEMA_VERSION}"))
            .join(format!("network={}", self.identity.network))
            .join(format!(
                "venue={}",
                encode_partition_value(&self.identity.venue)
            ))
            .join(format!(
                "coin={}",
                encode_partition_value(&self.identity.coin)
            ))
            .join(format!("settlement_date_utc={date}")))
    }
}

fn validate_identity_component(
    field: &'static str,
    value: &str,
) -> Result<(), StorageContractError> {
    if value.is_empty() {
        return Err(StorageContractError::EmptyIdentityField { field });
    }
    Ok(())
}

fn validate_decimal_scale(field: &'static str, value: Decimal) -> Result<(), StorageContractError> {
    if value.scale() > DECIMAL_SCALE {
        return Err(StorageContractError::DecimalScaleExceeded {
            field,
            actual: value.scale(),
            maximum: DECIMAL_SCALE,
        });
    }
    Ok(())
}

pub(crate) fn encode_partition_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    encoded
}

/// A row or partition violates the frozen settled-funding storage contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageContractError {
    /// The source request has its bounds in reverse order.
    ReversedRequestWindow {
        /// Inclusive requested start time.
        start_ms: i64,
        /// Inclusive requested end time.
        end_ms: i64,
    },
    /// A deterministic identity component is empty.
    EmptyIdentityField {
        /// Name of the empty identity field.
        field: &'static str,
    },
    /// A decimal would require rounding to fit schema v1.
    DecimalScaleExceeded {
        /// Name of the offending decimal field.
        field: &'static str,
        /// Decimal scale carried by the value.
        actual: u32,
        /// Maximum scale permitted by schema v1.
        maximum: u32,
    },
    /// A response observation does not belong to its recorded request window.
    ObservationOutsideRequestWindow {
        /// Settlement time falling outside the window.
        settlement_time_ms: i64,
        /// Inclusive start of the recorded request window.
        start_ms: i64,
        /// Inclusive end of the recorded request window.
        end_ms: i64,
    },
    /// A timestamp cannot be converted to the UTC partition date.
    TimestampOutOfRange {
        /// Settlement timestamp that could not be represented.
        timestamp_ms: i64,
    },
}

impl fmt::Display for StorageContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReversedRequestWindow { start_ms, end_ms } => write!(
                formatter,
                "request window starts at {start_ms}ms after it ends at {end_ms}ms"
            ),
            Self::EmptyIdentityField { field } => {
                write!(formatter, "settled-funding identity field {field} is empty")
            }
            Self::DecimalScaleExceeded {
                field,
                actual,
                maximum,
            } => write!(
                formatter,
                "{field} uses decimal scale {actual}, exceeding schema-v1 maximum {maximum}"
            ),
            Self::ObservationOutsideRequestWindow {
                settlement_time_ms,
                start_ms,
                end_ms,
            } => write!(
                formatter,
                "settlement {settlement_time_ms}ms is outside inclusive request window {start_ms}..={end_ms}ms"
            ),
            Self::TimestampOutOfRange { timestamp_ms } => write!(
                formatter,
                "settlement timestamp {timestamp_ms}ms cannot be represented as a UTC partition date"
            ),
        }
    }
}

impl Error for StorageContractError {}

#[cfg(test)]
mod tests {
    use super::*;
    use parquet::{basic::LogicalType, schema::types::SchemaDescriptor};
    use std::str::FromStr;

    fn decimal(value: &str) -> Decimal {
        Decimal::from_str(value).expect("valid test decimal")
    }

    fn provenance(window: RequestWindow) -> IngestionProvenance {
        IngestionProvenance {
            source_endpoint_class: SourceEndpointClass::Official,
            ingestion_time: TimestampMs::new(1_683_850_000_000),
            request_window: window,
            software_version: "0.0.0-test".to_owned(),
        }
    }

    fn entry(coin: &str, time: i64) -> FundingHistoryEntry {
        FundingHistoryEntry {
            coin: coin.to_owned(),
            funding_rate: decimal("-0.0006133368"),
            premium: decimal("-0.0009133368"),
            time: TimestampMs::new(time),
        }
    }

    #[test]
    fn schema_v1_is_valid_parquet_with_expected_identity_and_provenance_columns() {
        let schema = parquet_schema().expect("frozen schema parses");
        let descriptor = SchemaDescriptor::new(schema);
        let names: Vec<_> = descriptor
            .columns()
            .iter()
            .map(|column| column.name())
            .collect();

        assert_eq!(descriptor.num_columns(), 12);
        assert_eq!(
            &names[2..6],
            &["network", "venue", "coin", "settlement_time_ms"]
        );
        assert!(names.contains(&"source_endpoint_class"));
        assert!(names.contains(&"ingestion_time_ms"));
        assert!(names.contains(&"request_start_time_ms"));
        assert!(names.contains(&"request_end_time_ms"));
        let funding_rate_column = descriptor.column(6);
        let Some(LogicalType::Decimal(decimal)) = funding_rate_column.logical_type_ref() else {
            panic!("funding_rate must use the Parquet decimal logical type");
        };
        assert_eq!(decimal.scale, 18);
        assert_eq!(decimal.precision, 38);
    }

    #[test]
    fn record_preserves_identity_provenance_and_deterministic_partition() {
        let window = RequestWindow::new(
            TimestampMs::new(1_683_849_600_000),
            TimestampMs::new(1_683_853_200_000),
        )
        .expect("valid window");
        let record = SettledFundingRecord::from_history_entry(
            Network::Testnet,
            "hyper/liquid",
            entry("BTC:PERP", 1_683_849_600_048),
            provenance(window),
        )
        .expect("valid record");

        assert_eq!(record.schema_version, 1);
        assert_eq!(record.identity.network, Network::Testnet);
        assert_eq!(record.identity.venue, "hyper/liquid");
        assert_eq!(record.identity.coin, "BTC:PERP");
        assert_eq!(record.provenance.request_window, window);
        assert_eq!(record.provenance.software_version, "0.0.0-test");
        assert_eq!(
            record
                .partition_directory()
                .expect("timestamp has a UTC date"),
            PathBuf::from(
                "settled_funding/schema_version=1/network=testnet/venue=hyper%2Fliquid/coin=BTC%3APERP/settlement_date_utc=2023-05-12"
            )
        );
    }

    #[test]
    fn request_window_is_inclusive_but_rejects_reversed_bounds() {
        let instant = TimestampMs::new(10);
        let window = RequestWindow::new(instant, instant).expect("equal bounds are valid");
        assert!(window.contains(instant));

        assert_eq!(
            RequestWindow::new(TimestampMs::new(11), TimestampMs::new(10)),
            Err(StorageContractError::ReversedRequestWindow {
                start_ms: 11,
                end_ms: 10,
            })
        );
    }

    #[test]
    fn record_rejects_observations_outside_the_request_window() {
        let window =
            RequestWindow::new(TimestampMs::new(100), TimestampMs::new(200)).expect("valid window");
        let error = SettledFundingRecord::from_history_entry(
            Network::Mainnet,
            "hyperliquid",
            entry("BTC", 99),
            provenance(window),
        )
        .expect_err("observation provenance must be truthful");

        assert_eq!(
            error,
            StorageContractError::ObservationOutsideRequestWindow {
                settlement_time_ms: 99,
                start_ms: 100,
                end_ms: 200,
            }
        );
    }

    #[test]
    fn record_rejects_decimal_values_that_would_be_rounded() {
        let window =
            RequestWindow::new(TimestampMs::new(0), TimestampMs::new(100)).expect("valid window");
        let mut high_scale = entry("BTC", 50);
        high_scale.funding_rate = decimal("0.0000000000000000001");

        let error = SettledFundingRecord::from_history_entry(
            Network::Mainnet,
            "hyperliquid",
            high_scale,
            provenance(window),
        )
        .expect_err("schema conversion must never round silently");

        assert_eq!(
            error,
            StorageContractError::DecimalScaleExceeded {
                field: "funding_rate",
                actual: 19,
                maximum: 18,
            }
        );
    }
}
