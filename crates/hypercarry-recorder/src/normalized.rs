use crate::raw::{RawCaptureRecord, RawEvent};
use arrow_array::{
    ArrayRef, Decimal128Array, Int32Array, RecordBatch, StringArray, TimestampMillisecondArray,
    UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use parquet::{arrow::ArrowWriter, errors::ParquetError};
use rust_decimal::Decimal;
use serde_json::Value;
use std::{error::Error, fmt, fs::File, path::Path, str::FromStr, sync::Arc};

/// Stable schema version for normalized live-market Parquet rows.
pub const NORMALIZED_SCHEMA_VERSION: u32 = 1;
const DECIMAL_PRECISION: u8 = 38;
const DECIMAL_SCALE: i8 = 18;

/// Query-oriented projection of one supported public market-data frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedEvent {
    /// Version of the normalized projection contract.
    pub schema_version: u32,
    /// Lowercase network name of the source session.
    pub network: String,
    /// Source subscription channel, such as `activeAssetCtx` or `l2Book`.
    pub channel: String,
    /// Perpetual market symbol.
    pub coin: String,
    /// Frame receive time, unix milliseconds.
    pub received_at_ms: i64,
    /// Venue-reported source time, when the frame carried one.
    pub source_time_ms: Option<i64>,
    /// Transport connection that delivered the frame.
    pub connection_id: u64,
    /// Monotonic per-session receive sequence number.
    pub sequence: u64,
    /// Stable identity used to deduplicate replayed frames.
    pub event_id: String,
    /// Hourly funding rate, when present in the frame.
    pub funding: Option<Decimal>,
    /// Aggregate open interest, when present in the frame.
    pub open_interest: Option<Decimal>,
    /// Oracle price, when present in the frame.
    pub oracle_px: Option<Decimal>,
    /// Perpetual mark price, when present in the frame.
    pub mark_px: Option<Decimal>,
    /// Book midpoint, when present in the frame.
    pub mid_px: Option<Decimal>,
    /// Best bid price, when present in the frame.
    pub best_bid_px: Option<Decimal>,
    /// Best bid size, when present in the frame.
    pub best_bid_sz: Option<Decimal>,
    /// Best ask price, when present in the frame.
    pub best_ask_px: Option<Decimal>,
    /// Best ask size, when present in the frame.
    pub best_ask_sz: Option<Decimal>,
    /// Full canonical JSON payload retained for future projections.
    pub payload_json: String,
}

impl NormalizedEvent {
    /// Parse a supported frame without modifying the immutable raw record.
    /// Subscription acknowledgements, heartbeats, and unknown channels return
    /// `Ok(None)` and remain available in raw capture.
    ///
    /// # Errors
    ///
    /// Returns a typed parse error when a subscribed frame is malformed.
    pub fn from_raw(record: &RawCaptureRecord) -> Result<Option<Self>, NormalizeError> {
        let RawEvent::Frame { payload } = &record.event else {
            return Ok(None);
        };
        let value: Value = serde_json::from_str(payload).map_err(NormalizeError::Json)?;
        let channel = value
            .get("channel")
            .and_then(Value::as_str)
            .ok_or(NormalizeError::Missing("channel"))?;
        if matches!(channel, "subscriptionResponse" | "pong") {
            return Ok(None);
        }
        if !matches!(channel, "activeAssetCtx" | "l2Book") {
            return Ok(None);
        }

        let data = value.get("data").ok_or(NormalizeError::Missing("data"))?;
        let coin = data
            .get("coin")
            .and_then(Value::as_str)
            .ok_or(NormalizeError::Missing("data.coin"))?;
        let source_time_ms = data.get("time").and_then(Value::as_i64);
        let payload_json = serde_json::to_string(&value).map_err(NormalizeError::Json)?;
        let event_id = source_time_ms.map_or_else(
            || format!("{channel}|{coin}|{payload_json}"),
            |time| format!("{channel}|{coin}|{time}|{payload_json}"),
        );

        let mut event = Self {
            schema_version: NORMALIZED_SCHEMA_VERSION,
            network: record.network.as_str().to_owned(),
            channel: channel.to_owned(),
            coin: coin.to_owned(),
            received_at_ms: record.received_at_ms,
            source_time_ms,
            connection_id: record.connection_id,
            sequence: record.sequence,
            event_id,
            funding: None,
            open_interest: None,
            oracle_px: None,
            mark_px: None,
            mid_px: None,
            best_bid_px: None,
            best_bid_sz: None,
            best_ask_px: None,
            best_ask_sz: None,
            payload_json,
        };

        match channel {
            "activeAssetCtx" => populate_asset_context(&mut event, data)?,
            "l2Book" => populate_top_of_book(&mut event, data)?,
            _ => unreachable!("supported channels were checked above"),
        }
        Ok(Some(event))
    }
}

fn populate_asset_context(event: &mut NormalizedEvent, data: &Value) -> Result<(), NormalizeError> {
    let context = data.get("ctx").ok_or(NormalizeError::Missing("data.ctx"))?;
    event.funding = decimal_field(context, "funding")?;
    event.open_interest = decimal_field(context, "openInterest")?;
    event.oracle_px = decimal_field(context, "oraclePx")?;
    event.mark_px = decimal_field(context, "markPx")?;
    event.mid_px = decimal_field(context, "midPx")?;
    Ok(())
}

fn populate_top_of_book(event: &mut NormalizedEvent, data: &Value) -> Result<(), NormalizeError> {
    let levels = data
        .get("levels")
        .and_then(Value::as_array)
        .ok_or(NormalizeError::Missing("data.levels"))?;
    if let Some(bid) = levels
        .first()
        .and_then(Value::as_array)
        .and_then(|side| side.first())
    {
        event.best_bid_px = decimal_field(bid, "px")?;
        event.best_bid_sz = decimal_field(bid, "sz")?;
    }
    if let Some(ask) = levels
        .get(1)
        .and_then(Value::as_array)
        .and_then(|side| side.first())
    {
        event.best_ask_px = decimal_field(ask, "px")?;
        event.best_ask_sz = decimal_field(ask, "sz")?;
    }
    Ok(())
}

fn decimal_field(value: &Value, field: &'static str) -> Result<Option<Decimal>, NormalizeError> {
    let Some(raw) = value.get(field) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let text = raw.as_str().map_or_else(|| raw.to_string(), str::to_owned);
    Decimal::from_str(&text)
        .map(Some)
        .map_err(|source| NormalizeError::Decimal { field, source })
}

/// Incremental bounded-batch writer for normalized Parquet rows.
pub struct NormalizedParquetWriter {
    writer: ArrowWriter<File>,
    pending: Vec<NormalizedEvent>,
    batch_size: usize,
}

impl NormalizedParquetWriter {
    /// Create a new Parquet session, refusing to overwrite existing output.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero batch size or if the Parquet file cannot be
    /// created and initialized.
    pub fn create(path: &Path, batch_size: usize) -> Result<Self, NormalizeError> {
        if batch_size == 0 {
            return Err(NormalizeError::ZeroBatchSize);
        }
        let file = File::options()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(NormalizeError::Io)?;
        let writer = ArrowWriter::try_new(file, normalized_schema(), None)
            .map_err(NormalizeError::Parquet)?;
        Ok(Self {
            writer,
            pending: Vec::with_capacity(batch_size),
            batch_size,
        })
    }

    /// Buffer one event and write a row group when the configured bound is met.
    ///
    /// # Errors
    ///
    /// Returns a decimal, Arrow, or Parquet error if a full batch cannot be
    /// represented exactly and written.
    pub fn push(&mut self, event: NormalizedEvent) -> Result<(), NormalizeError> {
        self.pending.push(event);
        if self.pending.len() >= self.batch_size {
            self.flush()?;
        }
        Ok(())
    }

    /// Finalize a valid Parquet footer after flushing the last partial batch.
    ///
    /// # Errors
    ///
    /// Returns an Arrow or Parquet error if the remaining batch or footer
    /// cannot be written.
    pub fn finish(mut self) -> Result<(), NormalizeError> {
        self.flush()?;
        self.writer.close().map_err(NormalizeError::Parquet)?;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), NormalizeError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let batch = events_to_batch(&self.pending)?;
        self.writer.write(&batch).map_err(NormalizeError::Parquet)?;
        self.pending.clear();
        Ok(())
    }
}

/// Frozen Arrow schema used by normalized market-data Parquet v1.
pub fn normalized_schema() -> Arc<Schema> {
    let decimal = DataType::Decimal128(DECIMAL_PRECISION, DECIMAL_SCALE);
    Arc::new(Schema::new(vec![
        Field::new("schema_version", DataType::Int32, false),
        Field::new("network", DataType::Utf8, false),
        Field::new("channel", DataType::Utf8, false),
        Field::new("coin", DataType::Utf8, false),
        Field::new(
            "received_at",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
        Field::new(
            "source_time",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true,
        ),
        Field::new("connection_id", DataType::UInt64, false),
        Field::new("sequence", DataType::UInt64, false),
        Field::new("event_id", DataType::Utf8, false),
        Field::new("funding", decimal.clone(), true),
        Field::new("open_interest", decimal.clone(), true),
        Field::new("oracle_px", decimal.clone(), true),
        Field::new("mark_px", decimal.clone(), true),
        Field::new("mid_px", decimal.clone(), true),
        Field::new("best_bid_px", decimal.clone(), true),
        Field::new("best_bid_sz", decimal.clone(), true),
        Field::new("best_ask_px", decimal.clone(), true),
        Field::new("best_ask_sz", decimal, true),
        Field::new("payload_json", DataType::Utf8, false),
    ]))
}

fn events_to_batch(events: &[NormalizedEvent]) -> Result<RecordBatch, NormalizeError> {
    let decimal_array = |values: Vec<Option<Decimal>>| -> Result<ArrayRef, NormalizeError> {
        let values = values
            .into_iter()
            .map(|value| value.map(decimal_to_i128).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        let array = Decimal128Array::from(values)
            .with_precision_and_scale(DECIMAL_PRECISION, DECIMAL_SCALE)
            .map_err(NormalizeError::Arrow)?;
        Ok(Arc::new(array))
    };
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int32Array::from_iter_values(events.iter().map(|event| {
            i32::try_from(event.schema_version).expect("schema version fits i32")
        }))),
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|event| event.network.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|event| event.channel.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|event| event.coin.as_str()),
        )),
        Arc::new(
            TimestampMillisecondArray::from_iter_values(
                events.iter().map(|event| event.received_at_ms),
            )
            .with_timezone("UTC"),
        ),
        Arc::new(
            TimestampMillisecondArray::from(
                events
                    .iter()
                    .map(|event| event.source_time_ms)
                    .collect::<Vec<_>>(),
            )
            .with_timezone("UTC"),
        ),
        Arc::new(UInt64Array::from_iter_values(
            events.iter().map(|event| event.connection_id),
        )),
        Arc::new(UInt64Array::from_iter_values(
            events.iter().map(|event| event.sequence),
        )),
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|event| event.event_id.as_str()),
        )),
        decimal_array(events.iter().map(|event| event.funding).collect())?,
        decimal_array(events.iter().map(|event| event.open_interest).collect())?,
        decimal_array(events.iter().map(|event| event.oracle_px).collect())?,
        decimal_array(events.iter().map(|event| event.mark_px).collect())?,
        decimal_array(events.iter().map(|event| event.mid_px).collect())?,
        decimal_array(events.iter().map(|event| event.best_bid_px).collect())?,
        decimal_array(events.iter().map(|event| event.best_bid_sz).collect())?,
        decimal_array(events.iter().map(|event| event.best_ask_px).collect())?,
        decimal_array(events.iter().map(|event| event.best_ask_sz).collect())?,
        Arc::new(StringArray::from_iter_values(
            events.iter().map(|event| event.payload_json.as_str()),
        )),
    ];
    RecordBatch::try_new(normalized_schema(), columns).map_err(NormalizeError::Arrow)
}

fn decimal_to_i128(value: Decimal) -> Result<i128, NormalizeError> {
    if value.scale() > u32::from(DECIMAL_SCALE.unsigned_abs()) {
        return Err(NormalizeError::DecimalScale(value));
    }
    let exponent = u32::from(DECIMAL_SCALE.unsigned_abs()) - value.scale();
    value
        .mantissa()
        .checked_mul(10_i128.pow(exponent))
        .ok_or(NormalizeError::DecimalOverflow(value))
}

/// Raw-frame parsing or normalized Parquet failure.
#[derive(Debug)]
pub enum NormalizeError {
    /// A frame body was not valid JSON.
    Json(serde_json::Error),
    /// A required frame field was absent.
    Missing(&'static str),
    /// A frame field could not be parsed as a decimal.
    Decimal {
        /// Name of the offending field.
        field: &'static str,
        /// Underlying decimal parse error.
        source: rust_decimal::Error,
    },
    /// A decimal exceeded the normalized column scale.
    DecimalScale(Decimal),
    /// A decimal exceeded the normalized column precision.
    DecimalOverflow(Decimal),
    /// A Parquet writer was configured with a zero batch size.
    ZeroBatchSize,
    /// Writing the normalized Parquet output failed.
    Io(std::io::Error),
    /// Building the Arrow record batch failed.
    Arrow(arrow_schema::ArrowError),
    /// Encoding the Parquet file failed.
    Parquet(ParquetError),
}

impl fmt::Display for NormalizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "market frame JSON is invalid: {error}"),
            Self::Missing(field) => write!(formatter, "market frame is missing {field}"),
            Self::Decimal { field, source } => {
                write!(formatter, "market frame {field} is not a decimal: {source}")
            }
            Self::DecimalScale(value) => write!(
                formatter,
                "decimal {value} exceeds normalized scale {DECIMAL_SCALE}"
            ),
            Self::DecimalOverflow(value) => {
                write!(formatter, "decimal {value} exceeds normalized precision")
            }
            Self::ZeroBatchSize => {
                formatter.write_str("normalized Parquet batch size must be positive")
            }
            Self::Io(error) => write!(formatter, "normalized Parquet I/O failed: {error}"),
            Self::Arrow(error) => write!(formatter, "normalized Arrow batch failed: {error}"),
            Self::Parquet(error) => write!(formatter, "normalized Parquet write failed: {error}"),
        }
    }
}

impl Error for NormalizeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Decimal { source, .. } => Some(source),
            Self::Io(error) => Some(error),
            Self::Arrow(error) => Some(error),
            Self::Parquet(error) => Some(error),
            Self::Missing(_)
            | Self::DecimalScale(_)
            | Self::DecimalOverflow(_)
            | Self::ZeroBatchSize => None,
        }
    }
}
