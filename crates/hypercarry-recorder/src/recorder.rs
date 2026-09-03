mod capture;
mod config;
mod error;
mod pipeline;
mod session;

pub use config::{BackpressurePolicy, RecorderConfig, RecorderTiming};
pub use error::RecorderError;
pub use session::RecorderPaths;

use crate::{
    diagnostics::{DIAGNOSTICS_SCHEMA_VERSION, RecorderDiagnostics},
    normalized::NormalizedParquetWriter,
    raw::RawCaptureWriter,
    transport::MarketTransport,
};
use pipeline::normalize_stream;
use session::timestamp_ms;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Live recorder over an injectable reconnectable transport.
pub struct Recorder<T> {
    pub(super) config: RecorderConfig,
    pub(super) transport: T,
}

impl<T: MarketTransport> Recorder<T> {
    /// Couple validated options to a transport with matching network identity.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if the transport and dataset network
    /// identities differ.
    pub fn new(config: RecorderConfig, transport: T) -> Result<Self, RecorderError> {
        if config.network != transport.network() {
            return Err(RecorderError::Configuration(format!(
                "recorder network {} does not match transport network {}",
                config.network,
                transport.network()
            )));
        }
        Ok(Self { config, transport })
    }

    /// Record until cancellation or unrecoverable storage/transport exhaustion.
    ///
    /// # Errors
    ///
    /// Returns a typed error for session creation, raw or normalized storage,
    /// clock failure, task failure, or exhausted reconnect attempts.
    pub async fn run(
        &self,
        cancellation: CancellationToken,
    ) -> Result<RecorderDiagnostics, RecorderError> {
        let started_at_ms = timestamp_ms()?;
        let session_id = format!("session-{started_at_ms}-{}", std::process::id());
        let paths = RecorderPaths::for_session(&self.config, &session_id);
        paths.create_parents().await?;

        let mut raw_writer = RawCaptureWriter::create(&paths.raw_capture).await?;
        let normalized_writer =
            NormalizedParquetWriter::create(&paths.normalized_parquet, self.config.batch_size)?;
        let (sender, receiver) = mpsc::channel(self.config.queue_capacity);
        let stale_after = self.config.stale_after;
        let duplicate_window = self.config.duplicate_window;
        let normalizer_task = tokio::spawn(async move {
            normalize_stream(receiver, normalized_writer, stale_after, duplicate_window).await
        });

        let capture = self
            .capture(&session_id, &mut raw_writer, sender, &cancellation)
            .await;
        raw_writer.finish().await?;
        let normalization_report = normalizer_task
            .await
            .map_err(|error| RecorderError::Task(error.to_string()))??;
        let capture = capture?;
        let ended_at_ms = timestamp_ms()?;

        Ok(RecorderDiagnostics {
            schema_version: DIAGNOSTICS_SCHEMA_VERSION,
            session_id,
            network: self.config.network,
            started_at_ms,
            ended_at_ms,
            queue_capacity: self.config.queue_capacity,
            backpressure_policy: self.config.backpressure_policy.as_str(),
            connections: capture.connections,
            reconnects: capture.reconnects,
            heartbeats_sent: capture.heartbeats_sent,
            raw_frames: capture.raw_frames,
            normalized_frames: normalization_report.normalized_frames,
            dropped_normalized_frames: capture.dropped_normalized_frames,
            duplicate_frames: normalization_report.duplicate_frames,
            out_of_order_frames: normalization_report.out_of_order_frames,
            stale_frames: capture.stale_connections + normalization_report.stale_frames,
            parse_errors: normalization_report.parse_errors,
            raw_capture_path: paths.raw_capture.to_string_lossy().into_owned(),
            normalized_parquet_path: paths.normalized_parquet.to_string_lossy().into_owned(),
        })
    }
}
