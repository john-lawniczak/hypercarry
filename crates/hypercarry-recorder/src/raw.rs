use hypercarry_core::info::Network;
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, path::Path};
use tokio::{
    fs::File,
    io::{AsyncWriteExt, BufWriter},
};

/// Stable schema version for the append-only JSON-lines raw capture.
pub const RAW_SCHEMA_VERSION: u32 = 1;

/// One transport event in a raw recording session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RawEvent {
    /// Exact WebSocket text payload received from the server.
    Frame {
        /// Verbatim text frame as delivered by the venue.
        payload: String,
    },
    /// Connection boundary retained so replay can reproduce reconnect behavior.
    Disconnect {
        /// Human-readable reason the connection ended.
        reason: String,
    },
    /// A connection was recycled after receiving no server traffic in time.
    Stale {
        /// Silence duration that triggered the recycle, milliseconds.
        silence_ms: u64,
    },
}

/// Versioned raw record written before normalization is attempted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawCaptureRecord {
    /// Version of the raw capture contract.
    pub schema_version: u32,
    /// Identifier shared by every record in the session.
    pub session_id: String,
    /// Hyperliquid deployment that produced the event.
    pub network: Network,
    /// Transport connection that delivered the event.
    pub connection_id: u64,
    /// Monotonic per-session receive sequence number.
    pub sequence: u64,
    /// Event receive time, unix milliseconds.
    pub received_at_ms: i64,
    /// The captured transport event.
    #[serde(flatten)]
    pub event: RawEvent,
}

impl RawCaptureRecord {
    /// Decode one JSON line while enforcing the supported raw schema version.
    ///
    /// # Errors
    ///
    /// Returns a JSON error for malformed records or [`RawCaptureError::SchemaVersion`]
    /// for a record from an unsupported schema.
    pub fn from_json_line(line: &str) -> Result<Self, RawCaptureError> {
        let value: serde_json::Value = serde_json::from_str(line).map_err(RawCaptureError::Json)?;
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or(RawCaptureError::MissingSchemaVersion)?;
        if version != RAW_SCHEMA_VERSION {
            return Err(RawCaptureError::SchemaVersion(version));
        }
        serde_json::from_value(value).map_err(RawCaptureError::Json)
    }
}

/// Append-only async writer for raw JSON-lines capture.
pub struct RawCaptureWriter {
    writer: BufWriter<File>,
}

impl RawCaptureWriter {
    /// Create a new capture file, refusing to overwrite an existing session.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the session file cannot be created.
    pub async fn create(path: &Path) -> Result<Self, RawCaptureError> {
        let file = File::options()
            .create_new(true)
            .write(true)
            .open(path)
            .await
            .map_err(RawCaptureError::Io)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    /// Append one complete record before its frame enters normalization.
    ///
    /// # Errors
    ///
    /// Returns a serialization or I/O error when the record cannot be appended.
    pub async fn write(&mut self, record: &RawCaptureRecord) -> Result<(), RawCaptureError> {
        let encoded = serde_json::to_vec(record).map_err(RawCaptureError::Json)?;
        self.writer
            .write_all(&encoded)
            .await
            .map_err(RawCaptureError::Io)?;
        self.writer
            .write_all(b"\n")
            .await
            .map_err(RawCaptureError::Io)?;
        self.writer.flush().await.map_err(RawCaptureError::Io)
    }

    /// Flush the capture at a clean session boundary.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when buffered data cannot be flushed.
    pub async fn finish(mut self) -> Result<(), RawCaptureError> {
        self.writer.flush().await.map_err(RawCaptureError::Io)
    }
}

/// Raw capture read/write failure.
#[derive(Debug)]
pub enum RawCaptureError {
    /// Reading or writing the capture file failed.
    Io(std::io::Error),
    /// A capture line was not valid JSON.
    Json(serde_json::Error),
    /// A capture line omitted its schema version.
    MissingSchemaVersion,
    /// A capture line declared an unsupported schema version.
    SchemaVersion(u32),
}

impl fmt::Display for RawCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "raw capture I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "raw capture JSON is invalid: {error}"),
            Self::MissingSchemaVersion => {
                formatter.write_str("raw capture schema_version is missing")
            }
            Self::SchemaVersion(version) => write!(
                formatter,
                "raw capture schema version {version} is unsupported; expected {RAW_SCHEMA_VERSION}"
            ),
        }
    }
}

impl Error for RawCaptureError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::MissingSchemaVersion | Self::SchemaVersion(_) => None,
        }
    }
}
