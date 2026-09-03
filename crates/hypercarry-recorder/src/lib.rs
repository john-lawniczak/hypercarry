//! Bounded public Hyperliquid market-data recording and deterministic replay.
//!
//! Raw receive-order capture is the durable source of truth. Normalization is
//! a separate, versioned Parquet projection that can be rebuilt after parser
//! changes. Recorder diagnostics are also independently versioned.

#![warn(missing_docs)]

/// Versioned recorder health and throughput diagnostics.
pub mod diagnostics;
/// Rebuildable normalized Parquet projection of captured frames.
pub mod normalized;
/// Durable receive-order raw capture, the recorder's source of truth.
pub mod raw;
/// Bounded live capture pipeline wiring transport, capture, and normalization.
pub mod recorder;
/// Deterministic offline replay of a raw capture file.
pub mod replay;
/// Public Hyperliquid market-data WebSocket transport abstraction.
pub mod transport;

pub use diagnostics::{DIAGNOSTICS_SCHEMA_VERSION, RecorderDiagnostics};
pub use normalized::{NORMALIZED_SCHEMA_VERSION, NormalizedEvent, NormalizedParquetWriter};
pub use raw::{RAW_SCHEMA_VERSION, RawCaptureRecord, RawCaptureWriter, RawEvent};
pub use recorder::{
    BackpressurePolicy, Recorder, RecorderConfig, RecorderError, RecorderPaths, RecorderTiming,
};
pub use replay::{ReplayEvent, ReplayTransport};
pub use transport::{HyperliquidWebSocketTransport, MarketConnection, MarketTransport};
