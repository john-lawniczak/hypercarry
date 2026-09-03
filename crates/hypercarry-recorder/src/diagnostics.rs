use hypercarry_core::info::Network;
use serde::Serialize;

/// Stable schema version for recorder health and completion diagnostics.
pub const DIAGNOSTICS_SCHEMA_VERSION: u32 = 1;

/// Stable, machine-readable health summary for one recording or replay session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecorderDiagnostics {
    /// Version of this diagnostics contract.
    pub schema_version: u32,
    /// Identifier shared by every artifact of the session.
    pub session_id: String,
    /// Hyperliquid deployment recorded or replayed.
    pub network: Network,
    /// Session start time, unix milliseconds.
    pub started_at_ms: i64,
    /// Session end time, unix milliseconds.
    pub ended_at_ms: i64,
    /// Configured normalization queue capacity.
    pub queue_capacity: usize,
    /// Name of the active backpressure policy.
    pub backpressure_policy: &'static str,
    /// Successful transport connections established.
    pub connections: u64,
    /// Reconnections after a dropped or stale connection.
    pub reconnects: u64,
    /// Application heartbeats sent to the venue.
    pub heartbeats_sent: u64,
    /// Raw frames durably captured.
    pub raw_frames: u64,
    /// Frames normalized into the Parquet projection.
    pub normalized_frames: u64,
    /// Normalized frames dropped under backpressure.
    pub dropped_normalized_frames: u64,
    /// Exact-duplicate frames observed during normalization.
    pub duplicate_frames: u64,
    /// Frames whose timestamps regressed relative to their predecessor.
    pub out_of_order_frames: u64,
    /// Frames rejected because their source data was stale.
    pub stale_frames: u64,
    /// Frames that failed to parse during normalization.
    pub parse_errors: u64,
    /// Path to the raw capture file.
    pub raw_capture_path: String,
    /// Path to the normalized Parquet output.
    pub normalized_parquet_path: String,
}
