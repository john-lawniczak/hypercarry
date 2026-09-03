use crate::{
    config::{OutputFormat, PredictOptions},
    error::{CliError, ErrorCategory},
};
use chrono::{DateTime, SecondsFormat, Utc};
use hypercarry_core::{
    predictor::{
        FUNDING_HOUR_MS, FundingPrediction, OfficialBenchmark, PredictionEvaluation,
        PredictionRequest, PredictorError, PremiumReplay, PremiumSample, RealizedSettlement,
        evaluate_prediction, predict_next_hour,
    },
    types::TimestampMs,
};
use hypercarry_recorder::{RawCaptureRecord, ReplayEvent, ReplayTransport};
use hypercarry_storage::dataset::SettledFundingDataset;
use serde::Serialize;
use std::{
    io::{self, Write},
    path::Path,
};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PredictionSnapshot {
    pub schema_version: u32,
    pub network: hypercarry_core::info::Network,
    pub capture_path: String,
    pub raw_frames_replayed: u64,
    pub premium_samples_replayed: usize,
    pub prediction: FundingPrediction,
    pub evaluation: Option<PredictionEvaluation>,
}

pub fn run(options: &PredictOptions) -> Result<(), CliError> {
    let document = load_snapshot(options)?;
    let stdout = io::stdout();
    write_output(&mut stdout.lock(), &document, options.output)
}

pub(crate) fn load_snapshot(options: &PredictOptions) -> Result<PredictionSnapshot, CliError> {
    let mut feed = ReplaySampleFeed::open(&options.capture, options.network, options.coin.clone())?;
    load_snapshot_with_feed(options, &mut feed)
}

pub(crate) fn load_snapshot_with_feed(
    options: &PredictOptions,
    feed: &mut ReplaySampleFeed,
) -> Result<PredictionSnapshot, CliError> {
    load_snapshot_from_feed(options, feed, true)
}

pub(crate) fn load_live_snapshot_with_feed(
    options: &PredictOptions,
    feed: &mut ReplaySampleFeed,
) -> Result<PredictionSnapshot, CliError> {
    load_snapshot_from_feed(options, feed, false)
}

fn load_snapshot_from_feed(
    options: &PredictOptions,
    feed: &mut ReplaySampleFeed,
    include_evaluation: bool,
) -> Result<PredictionSnapshot, CliError> {
    let official_benchmark = options
        .official_rate
        .zip(options.official_observed_at_ms)
        .map(|(hourly_rate, observed_at_ms)| OfficialBenchmark {
            observed_at_ms: TimestampMs::new(observed_at_ms),
            settlement_time_ms: TimestampMs::new(options.settlement_ms),
            hourly_rate,
        });
    let request = PredictionRequest::new(
        options.coin.clone(),
        TimestampMs::new(options.settlement_ms),
        TimestampMs::new(options.as_of_ms),
        official_benchmark,
    )
    .map_err(predictor_error)?;
    let (samples, raw_frames_replayed) = if include_evaluation {
        feed.poll(
            request.window_start_ms().as_i64(),
            request.as_of_ms.as_i64(),
        )?
    } else {
        feed.poll_live(
            request.window_start_ms().as_i64(),
            request.as_of_ms.as_i64(),
        )?
    };
    let prediction = predict_next_hour(&request, samples).map_err(predictor_error)?;
    let evaluation = if include_evaluation {
        realized_settlement(options)?
            .map(|realized| evaluate_prediction(&prediction, &realized))
            .transpose()
            .map_err(predictor_error)?
    } else {
        None
    };

    Ok(PredictionSnapshot {
        schema_version: prediction.schema_version,
        network: options.network,
        capture_path: options.capture.to_string_lossy().into_owned(),
        raw_frames_replayed,
        premium_samples_replayed: samples.len(),
        prediction,
        evaluation,
    })
}

pub(crate) struct ReplaySampleFeed {
    replay: ReplayTransport,
    expected_network: hypercarry_core::info::Network,
    coin: String,
    samples: Vec<PremiumSample>,
    pending_frames: Vec<RawCaptureRecord>,
    raw_frames: u64,
    premium_replay: PremiumReplay,
}

impl ReplaySampleFeed {
    pub(crate) fn open(
        path: &Path,
        expected_network: hypercarry_core::info::Network,
        coin: String,
    ) -> Result<Self, CliError> {
        let replay = ReplayTransport::open(path).map_err(|error| {
            CliError::with_source(
                ErrorCategory::Storage,
                format!(
                    "could not open raw predictor capture {}; start `hypercarry record` and pass its raw_capture path",
                    path.display()
                ),
                error,
            )
        })?;
        Ok(Self {
            replay,
            expected_network,
            coin,
            samples: Vec::new(),
            pending_frames: Vec::new(),
            raw_frames: 0,
            premium_replay: PremiumReplay::new(),
        })
    }

    pub(crate) fn poll(
        &mut self,
        window_start_ms: i64,
        cutoff_ms: i64,
    ) -> Result<(&[PremiumSample], u64), CliError> {
        self.poll_inner(window_start_ms, cutoff_ms, false)
    }

    fn poll_live(
        &mut self,
        window_start_ms: i64,
        cutoff_ms: i64,
    ) -> Result<(&[PremiumSample], u64), CliError> {
        self.poll_inner(window_start_ms, cutoff_ms, true)
    }

    fn poll_inner(
        &mut self,
        window_start_ms: i64,
        cutoff_ms: i64,
        preserve_future: bool,
    ) -> Result<(&[PremiumSample], u64), CliError> {
        for record in std::mem::take(&mut self.pending_frames) {
            if record.received_at_ms > cutoff_ms {
                self.pending_frames.push(record);
            } else {
                self.ingest_record(record, window_start_ms)?;
            }
        }
        while let Some(event) = self.replay.next() {
            let event = event.map_err(|error| {
                CliError::with_source(
                    ErrorCategory::Schema,
                    "could not replay raw predictor capture; preserve the file and start a new recording session",
                    error,
                )
            })?;
            let ReplayEvent::Frame(record) = event else {
                continue;
            };
            if record.received_at_ms > cutoff_ms {
                if preserve_future {
                    self.pending_frames.push(record);
                }
                continue;
            }
            self.ingest_record(record, window_start_ms)?;
        }
        self.samples
            .retain(|sample| sample.observed_at_ms.as_i64() >= window_start_ms);
        Ok((&self.samples, self.raw_frames))
    }

    fn ingest_record(
        &mut self,
        record: RawCaptureRecord,
        window_start_ms: i64,
    ) -> Result<(), CliError> {
        self.raw_frames = self.raw_frames.saturating_add(1);
        if record.network != self.expected_network {
            return Err(CliError::new(
                ErrorCategory::Schema,
                format!(
                    "raw capture network {} does not match requested {}",
                    record.network, self.expected_network
                ),
            ));
        }
        let hypercarry_recorder::RawEvent::Frame { payload } = record.event else {
            unreachable!("ReplayEvent::Frame always contains RawEvent::Frame");
        };
        if let Some(sample) = self
            .premium_replay
            .ingest(&payload, TimestampMs::new(record.received_at_ms))
            .map_err(predictor_error)?
            && sample.coin == self.coin
            && sample.observed_at_ms.as_i64() >= window_start_ms
        {
            self.samples.push(sample);
        }
        Ok(())
    }
}

fn realized_settlement(options: &PredictOptions) -> Result<Option<RealizedSettlement>, CliError> {
    let records = SettledFundingDataset::new(&options.dataset)
        .stream_records(options.network, "hyperliquid", &options.coin)
        .map_err(|error| {
            CliError::with_source(
                ErrorCategory::Storage,
                format!(
                    "could not read {} settled funding for coin {:?}",
                    options.network, options.coin
                ),
                error,
            )
        })?;
    let mut matches = records.iter().filter(|record| {
        canonical_hour(record.identity.settlement_time.as_i64()) == options.settlement_ms
    });
    let Some(record) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(CliError::new(
            ErrorCategory::Schema,
            format!(
                "settled dataset contains multiple {:?} observations for UTC hour {}ms",
                options.coin, options.settlement_ms
            ),
        ));
    }
    RealizedSettlement::new(
        options.coin.clone(),
        TimestampMs::new(options.settlement_ms),
        record.funding_rate,
    )
    .map(Some)
    .map_err(predictor_error)
}

fn canonical_hour(timestamp_ms: i64) -> i64 {
    timestamp_ms - timestamp_ms.rem_euclid(FUNDING_HOUR_MS)
}

fn predictor_error(error: PredictorError) -> CliError {
    let category = match error {
        PredictorError::NoSamples { .. } => ErrorCategory::PartialData,
        PredictorError::Payload(_)
        | PredictorError::ConflictingSampleTimestamp(_)
        | PredictorError::NonPositivePrice { .. }
        | PredictorError::NonPositiveBookSize { .. } => ErrorCategory::Schema,
        PredictorError::Arithmetic => ErrorCategory::Internal,
        PredictorError::EmptyCoin
        | PredictorError::UnalignedSettlement(_)
        | PredictorError::CutoffOutsideWindow { .. }
        | PredictorError::FutureBenchmark { .. }
        | PredictorError::BenchmarkSettlementMismatch { .. }
        | PredictorError::MissingBenchmarkInterval
        | PredictorError::ZeroBenchmarkInterval
        | PredictorError::SettlementMismatch
        | PredictorError::PredictionAfterSettlement { .. }
        | PredictorError::EmptyBacktest
        | PredictorError::MixedBacktestCoins
        | PredictorError::DuplicateBacktestSettlement => ErrorCategory::Configuration,
    };
    CliError::with_source(category, "funding prediction failed", error)
}

fn write_output(
    output: &mut impl Write,
    document: &PredictionSnapshot,
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => write_human(output, document),
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *output, document).map_err(|error| {
                CliError::with_source(
                    ErrorCategory::Output,
                    "could not serialize prediction JSON",
                    error,
                )
            })?;
            writeln!(output).map_err(output_error)
        }
    }
}

fn write_human(output: &mut impl Write, document: &PredictionSnapshot) -> Result<(), CliError> {
    let prediction = &document.prediction;
    let result = (|| -> io::Result<()> {
        writeln!(
            output,
            "{}-PERP · HYPERLIQUID {}",
            prediction.coin,
            document.network.as_str().to_uppercase()
        )?;
        writeln!(output)?;
        writeln!(
            output,
            "Settlement UTC     {} ({} ms)",
            timestamp_utc(prediction.settlement_time_ms.as_i64()),
            prediction.settlement_time_ms.as_i64()
        )?;
        writeln!(
            output,
            "Cutoff UTC         {} ({} ms)",
            timestamp_utc(prediction.generated_at_ms.as_i64()),
            prediction.generated_at_ms.as_i64()
        )?;
        writeln!(
            output,
            "Predicted hourly   {}",
            prediction.predicted_hourly_rate.normalize()
        )?;
        writeln!(
            output,
            "Average premium    {}",
            prediction.average_premium.normalize()
        )?;
        writeln!(
            output,
            "Coverage           {}/{} ({})",
            prediction.coverage.samples_used,
            prediction.coverage.expected_samples_so_far,
            prediction.coverage.coverage_ratio.normalize()
        )?;
        writeln!(
            output,
            "Confidence         {}",
            prediction.coverage.confidence_ratio.normalize()
        )?;
        if let Some(benchmark) = prediction.official_benchmark_hourly_rate {
            writeln!(output, "Official benchmark {}", benchmark.normalize())?;
        }
        if let Some(evaluation) = &document.evaluation {
            writeln!(
                output,
                "Realized hourly    {}",
                evaluation.realized_hourly_rate.normalize()
            )?;
            writeln!(
                output,
                "Signed error       {}",
                evaluation.signed_error.normalize()
            )?;
            writeln!(
                output,
                "Absolute error     {}",
                evaluation.absolute_error.normalize()
            )?;
        } else {
            writeln!(
                output,
                "Realized error     unavailable (settlement not in dataset)"
            )?;
        }
        writeln!(
            output,
            "Raw frames         {}",
            document.raw_frames_replayed
        )?;
        writeln!(
            output,
            "Premium samples    {}",
            document.premium_samples_replayed
        )
    })();
    result.map_err(output_error)
}

fn output_error(error: io::Error) -> CliError {
    CliError::with_source(
        ErrorCategory::Output,
        "could not write prediction output",
        error,
    )
}

fn timestamp_utc(timestamp_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp_ms).map_or_else(
        || format!("invalid UTC timestamp {timestamp_ms}"),
        |timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercarry_core::info::Network;
    use hypercarry_recorder::{RAW_SCHEMA_VERSION, RawCaptureRecord, RawEvent};
    use serde_json::Value;
    use std::fs::OpenOptions;
    use tempfile::tempdir;

    fn raw_record(
        network: Network,
        sequence: u64,
        received_at_ms: i64,
        payload: &str,
    ) -> RawCaptureRecord {
        RawCaptureRecord {
            schema_version: RAW_SCHEMA_VERSION,
            session_id: "session-test".to_owned(),
            network,
            connection_id: 1,
            sequence,
            received_at_ms,
            event: RawEvent::Frame {
                payload: payload.to_owned(),
            },
        }
    }

    #[test]
    fn raw_replay_extracts_asset_context_and_rejects_network_mismatch() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("capture.jsonl");
        let context = r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"oraclePx":"100","funding":"0","openInterest":"1"}}}"#;
        let book = r#"{"channel":"l2Book","data":{"coin":"BTC","time":1002,"levels":[[{"px":"101","sz":"200","n":1}],[{"px":"102","sz":"200","n":1}]]}}"#;
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                serde_json::to_string(&raw_record(Network::Testnet, 1, 1_000, context)).unwrap(),
                serde_json::to_string(&raw_record(Network::Testnet, 2, 1_001, book)).unwrap(),
                serde_json::to_string(&raw_record(Network::Testnet, 3, 2_000, "{}")).unwrap()
            ),
        )
        .unwrap();

        let mut feed = ReplaySampleFeed::open(&path, Network::Testnet, "BTC".to_owned()).unwrap();
        let (samples, frames) = feed.poll(0, 1_001).unwrap();
        assert_eq!(frames, 2);
        assert_eq!(samples.len(), 1);

        let mut mismatched_feed =
            ReplaySampleFeed::open(&path, Network::Mainnet, "BTC".to_owned()).unwrap();
        let error = mismatched_feed
            .poll(0, 1_001)
            .expect_err("capture network identity must fail closed");
        assert_eq!(error.category(), ErrorCategory::Schema);
    }

    #[test]
    fn replay_feed_reads_frames_appended_after_initial_end_of_file() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("capture.jsonl");
        let context = r#"{"channel":"activeAssetCtx","data":{"coin":"BTC","ctx":{"oraclePx":"100","funding":"0","openInterest":"1"}}}"#;
        let book = r#"{"channel":"l2Book","data":{"coin":"BTC","time":1001,"levels":[[{"px":"101","sz":"200","n":1}],[{"px":"102","sz":"200","n":1}]]}}"#;
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::to_string(&raw_record(Network::Testnet, 1, 1_000, context)).unwrap()
            ),
        )
        .unwrap();
        let mut feed = ReplaySampleFeed::open(&path, Network::Testnet, "BTC".to_owned()).unwrap();
        let (samples, frames) = feed.poll(0, 1_001).unwrap();
        assert!(samples.is_empty());
        assert_eq!(frames, 1);

        let mut capture = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            capture,
            "{}",
            serde_json::to_string(&raw_record(Network::Testnet, 2, 1_002, book)).unwrap()
        )
        .unwrap();
        capture.flush().unwrap();

        let (samples, frames) = feed.poll_live(0, 1_001).unwrap();
        assert!(samples.is_empty());
        assert_eq!(frames, 1);

        let (samples, frames) = feed.poll_live(0, 1_002).unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(frames, 2);
    }

    #[test]
    fn json_output_keeps_prediction_and_optional_evaluation_distinct() {
        let request = PredictionRequest::new(
            "BTC",
            TimestampMs::new(FUNDING_HOUR_MS),
            TimestampMs::new(1_000),
            None,
        )
        .unwrap();
        let sample = PremiumSample::new(
            "BTC",
            TimestampMs::new(1_000),
            "100".parse().unwrap(),
            "101".parse().unwrap(),
            "102".parse().unwrap(),
            None,
        )
        .unwrap();
        let prediction = predict_next_hour(&request, &[sample]).unwrap();
        let document = PredictionSnapshot {
            schema_version: 1,
            network: Network::Testnet,
            capture_path: "capture.jsonl".to_owned(),
            raw_frames_replayed: 1,
            premium_samples_replayed: 1,
            prediction,
            evaluation: None,
        };
        let mut output = Vec::new();
        write_output(&mut output, &document, OutputFormat::Json).unwrap();
        let json: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(json["prediction"]["predicted_hourly_rate"], "0.0011875");
        assert!(json["evaluation"].is_null());
    }
}
