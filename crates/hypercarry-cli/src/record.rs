use crate::{
    config::{OutputFormat, RecordOptions},
    error::{CliError, ErrorCategory},
};
use hypercarry_recorder::{
    HyperliquidWebSocketTransport, Recorder, RecorderConfig, RecorderDiagnostics, RecorderError,
};
use std::io::{self, Write};
use tokio_util::sync::CancellationToken;

pub fn run(options: &RecordOptions) -> Result<(), CliError> {
    let config = RecorderConfig::new(options.network, options.coins.clone(), &options.dataset)
        .and_then(|config| config.with_queue_capacity(options.queue_capacity))
        .map_err(record_error)?;
    let transport = HyperliquidWebSocketTransport::for_network(options.network);
    let recorder = Recorder::new(config, transport).map_err(record_error)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            CliError::with_source(
                ErrorCategory::Internal,
                "could not initialize the async recorder runtime",
                error,
            )
        })?;
    let diagnostics = runtime.block_on(async {
        let cancellation = CancellationToken::new();
        let recording = recorder.run(cancellation.clone());
        tokio::pin!(recording);
        tokio::select! {
            result = &mut recording => result,
            signal = tokio::signal::ctrl_c() => {
                cancellation.cancel();
                let result = recording.await;
                if let Err(error) = signal {
                    return Err(RecorderError::Task(format!(
                        "could not install the Ctrl-C handler: {error}"
                    )));
                }
                result
            }
        }
    });
    let diagnostics = diagnostics.map_err(record_error)?;
    let stdout = io::stdout();
    write_diagnostics(&mut stdout.lock(), &diagnostics, options.output)
}

fn record_error(error: RecorderError) -> CliError {
    let (category, recovery) = match &error {
        RecorderError::Configuration(_) => (
            ErrorCategory::Configuration,
            "correct the recorder configuration and start a new session",
        ),
        RecorderError::ReconnectExhausted { .. } | RecorderError::Transport(_) => (
            ErrorCategory::Network,
            "verify network reachability and rerun record to start a new session",
        ),
        RecorderError::Normalize(_) => (
            ErrorCategory::Schema,
            "preserve the raw JSONL session; it can be replayed after parser repair",
        ),
        RecorderError::Io(_) | RecorderError::Raw(_) => (
            ErrorCategory::Storage,
            "verify dataset permissions and free space before starting a new session",
        ),
        RecorderError::Clock(_) | RecorderError::Task(_) => (
            ErrorCategory::Internal,
            "preserve the raw session and retry after correcting the reported host error",
        ),
    };
    CliError::with_source(
        category,
        format!("live recording failed; {recovery}"),
        error,
    )
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use hypercarry_recorder::normalized::NormalizeError;

    #[test]
    fn normalization_failure_preserves_raw_capture_for_replay() {
        let error = record_error(RecorderError::Normalize(NormalizeError::Missing(
            "data.coin",
        )));

        assert_eq!(error.category(), ErrorCategory::Schema);
        assert!(error.to_string().contains("preserve the raw JSONL session"));
        assert!(error.to_string().contains("replayed after parser repair"));
    }
}

fn write_diagnostics(
    output: &mut impl Write,
    diagnostics: &RecorderDiagnostics,
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => write_human(output, diagnostics),
        OutputFormat::Json => write_json(output, diagnostics),
    }
}

fn write_human(output: &mut impl Write, diagnostics: &RecorderDiagnostics) -> Result<(), CliError> {
    let result = (|| -> io::Result<()> {
        writeln!(output, "session: {}", diagnostics.session_id)?;
        writeln!(output, "network: {}", diagnostics.network)?;
        writeln!(output, "raw_frames: {}", diagnostics.raw_frames)?;
        writeln!(
            output,
            "normalized_frames: {}",
            diagnostics.normalized_frames
        )?;
        writeln!(
            output,
            "dropped_normalized_frames: {} ({})",
            diagnostics.dropped_normalized_frames, diagnostics.backpressure_policy
        )?;
        writeln!(output, "reconnects: {}", diagnostics.reconnects)?;
        writeln!(output, "duplicate_frames: {}", diagnostics.duplicate_frames)?;
        writeln!(
            output,
            "out_of_order_frames: {}",
            diagnostics.out_of_order_frames
        )?;
        writeln!(output, "stale_frames: {}", diagnostics.stale_frames)?;
        writeln!(output, "parse_errors: {}", diagnostics.parse_errors)?;
        writeln!(output, "raw_capture: {}", diagnostics.raw_capture_path)?;
        writeln!(
            output,
            "normalized_parquet: {}",
            diagnostics.normalized_parquet_path
        )
    })();
    result.map_err(output_error)
}

fn write_json(output: &mut impl Write, diagnostics: &RecorderDiagnostics) -> Result<(), CliError> {
    serde_json::to_writer_pretty(&mut *output, diagnostics).map_err(|error| {
        CliError::with_source(
            ErrorCategory::Output,
            "could not serialize record diagnostics JSON",
            error,
        )
    })?;
    writeln!(output).map_err(output_error)
}

fn output_error(error: io::Error) -> CliError {
    CliError::with_source(
        ErrorCategory::Output,
        "could not write record diagnostics",
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercarry_core::info::Network;
    use serde_json::Value;

    fn diagnostics() -> RecorderDiagnostics {
        RecorderDiagnostics {
            schema_version: 1,
            session_id: "session-1-2".to_owned(),
            network: Network::Testnet,
            started_at_ms: 1,
            ended_at_ms: 2,
            queue_capacity: 8,
            backpressure_policy: "drop_newest_normalized",
            connections: 2,
            reconnects: 1,
            heartbeats_sent: 3,
            raw_frames: 10,
            normalized_frames: 8,
            dropped_normalized_frames: 1,
            duplicate_frames: 1,
            out_of_order_frames: 1,
            stale_frames: 2,
            parse_errors: 0,
            raw_capture_path: "raw.jsonl".to_owned(),
            normalized_parquet_path: "normalized.parquet".to_owned(),
        }
    }

    #[test]
    fn record_diagnostics_have_stable_human_and_json_output() {
        let diagnostics = diagnostics();
        let mut human = Vec::new();
        write_diagnostics(&mut human, &diagnostics, OutputFormat::Human)
            .expect("human diagnostics render");
        let human = String::from_utf8(human).expect("human output is UTF-8");
        assert!(human.contains("dropped_normalized_frames: 1 (drop_newest_normalized)"));
        assert!(human.contains("normalized_parquet: normalized.parquet"));

        let mut json = Vec::new();
        write_diagnostics(&mut json, &diagnostics, OutputFormat::Json)
            .expect("JSON diagnostics render");
        let json: Value = serde_json::from_slice(&json).expect("diagnostics are JSON");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["network"], "testnet");
        assert_eq!(json["queue_capacity"], 8);
        assert_eq!(json["dropped_normalized_frames"], 1);
    }
}
