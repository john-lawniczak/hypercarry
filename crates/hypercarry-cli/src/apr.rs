use crate::{
    config::{AprOptions, OutputFormat},
    error::{CliError, ErrorCategory},
};
use chrono::{DateTime, SecondsFormat, Utc};
use hypercarry_core::metrics::{FundingInterval, funding_apr};
use hypercarry_storage::{dataset::SettledFundingDataset, settled_funding::SettledFundingRecord};
use rust_decimal::Decimal;
use serde::Serialize;
use std::io::{self, Write};

const VENUE: &str = "hyperliquid";
const JSON_SCHEMA_VERSION: u32 = 1;
const MILLISECONDS_PER_HOUR: i64 = 60 * 60 * 1_000;
const MAX_SETTLEMENT_JITTER_MS: i64 = 60 * 1_000;

#[derive(Debug, PartialEq, Eq, Serialize)]
struct AprView<'a> {
    schema_version: u32,
    network: hypercarry_core::info::Network,
    venue: &'static str,
    coin: &'a str,
    settlement_time_ms: i64,
    funding_interval_hours: u32,
    funding_rate: String,
    funding_apr: String,
    available_observations: usize,
    #[serde(skip)]
    settlement_time_iso: String,
    #[serde(skip)]
    funding_rate_percent: String,
    #[serde(skip)]
    funding_rate_bps: String,
    #[serde(skip)]
    funding_apr_percent: String,
    #[serde(skip)]
    contiguous_history_days: Option<usize>,
}

pub fn run(options: &AprOptions) -> Result<(), CliError> {
    let dataset = SettledFundingDataset::new(&options.dataset);
    let view = read_latest_apr(&dataset, options)?;
    let stdout = io::stdout();
    write_apr(&mut stdout.lock(), &view, options.output)
}

fn read_latest_apr<'a>(
    dataset: &SettledFundingDataset,
    options: &'a AprOptions,
) -> Result<AprView<'a>, CliError> {
    let records = dataset
        .stream_records(options.network, VENUE, &options.coin)
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
    let latest = records.last().ok_or_else(|| {
        CliError::new(
            ErrorCategory::PartialData,
            format!(
                "dataset contains no {} settled funding for coin {:?}; run backfill first",
                options.network, options.coin
            ),
        )
    })?;
    let interval = FundingInterval::from_hours(1).expect("one hour is non-zero");
    let apr = funding_apr(latest.funding_rate, interval);
    let settlement_time_iso =
        DateTime::<Utc>::from_timestamp_millis(latest.identity.settlement_time.as_i64())
            .ok_or_else(|| {
                CliError::new(
                    ErrorCategory::Schema,
                    format!(
                        "settlement timestamp {}ms cannot be formatted as UTC",
                        latest.identity.settlement_time.as_i64()
                    ),
                )
            })?
            .to_rfc3339_opts(SecondsFormat::Millis, true);
    Ok(AprView {
        schema_version: JSON_SCHEMA_VERSION,
        network: options.network,
        venue: VENUE,
        coin: &options.coin,
        settlement_time_ms: latest.identity.settlement_time.as_i64(),
        funding_interval_hours: interval.hours(),
        funding_rate: latest.funding_rate.normalize().to_string(),
        funding_apr: apr.normalize().to_string(),
        available_observations: records.len(),
        settlement_time_iso,
        funding_rate_percent: signed_scaled(latest.funding_rate, 100),
        funding_rate_bps: signed_scaled(latest.funding_rate, 10_000),
        funding_apr_percent: signed_scaled(apr, 100),
        contiguous_history_days: contiguous_history_days(&records),
    })
}

fn signed_scaled(value: Decimal, multiplier: i64) -> String {
    let scaled = (value * Decimal::from(multiplier)).normalize();
    if scaled.is_sign_negative() {
        scaled.to_string()
    } else {
        format!("+{scaled}")
    }
}

fn contiguous_history_days(records: &[SettledFundingRecord]) -> Option<usize> {
    let days = records.len().checked_div(24)?;
    if days == 0 || days * 24 != records.len() {
        return None;
    }
    records
        .windows(2)
        .all(|window| {
            let elapsed = window[1].identity.settlement_time.as_i64()
                - window[0].identity.settlement_time.as_i64();
            (elapsed - MILLISECONDS_PER_HOUR).abs() <= MAX_SETTLEMENT_JITTER_MS
        })
        .then_some(days)
}

fn write_apr(
    output: &mut impl Write,
    view: &AprView<'_>,
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => {
            let result = (|| -> io::Result<()> {
                writeln!(
                    output,
                    "{}-PERP · {} {}",
                    view.coin,
                    view.venue.to_ascii_uppercase(),
                    view.network.as_str().to_ascii_uppercase()
                )?;
                writeln!(output)?;
                writeln!(output, "{:<17}{}", "Settlement", view.settlement_time_iso)?;
                writeln!(
                    output,
                    "{:<17}{}%  ({} bps)",
                    "Hourly funding", view.funding_rate_percent, view.funding_rate_bps
                )?;
                writeln!(output, "{:<17}{}%", "Simple APR", view.funding_apr_percent)?;
                match view.contiguous_history_days {
                    Some(1) => writeln!(
                        output,
                        "{:<17}{} hourly observations (1 day)",
                        "History", view.available_observations
                    ),
                    Some(days) => writeln!(
                        output,
                        "{:<17}{} hourly observations ({days} days)",
                        "History", view.available_observations
                    ),
                    None if view.available_observations == 1 => {
                        writeln!(output, "{:<17}1 hourly observation", "History")
                    }
                    None => writeln!(
                        output,
                        "{:<17}{} hourly observations",
                        "History", view.available_observations
                    ),
                }
            })();
            result.map_err(output_error)
        }
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *output, view).map_err(|error| {
                CliError::with_source(ErrorCategory::Output, "could not serialize APR JSON", error)
            })?;
            writeln!(output).map_err(output_error)
        }
    }
}

fn output_error(error: io::Error) -> CliError {
    CliError::with_source(ErrorCategory::Output, "could not write APR output", error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TracingMode;
    use hypercarry_core::{
        info::Network,
        types::{FundingHistoryEntry, TimestampMs},
    };
    use hypercarry_storage::settled_funding::{
        IngestionProvenance, RequestWindow, SettledFundingRecord, SourceEndpointClass,
    };
    use serde_json::Value;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "hypercarry-apr-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory is unique");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn options(path: PathBuf) -> AprOptions {
        AprOptions {
            network: Network::Testnet,
            coin: "BTC".to_owned(),
            dataset: path,
            output: OutputFormat::Human,
            tracing: TracingMode::Quiet,
        }
    }

    fn record(time: i64, rate: &str) -> SettledFundingRecord {
        SettledFundingRecord::from_history_entry(
            Network::Testnet,
            VENUE,
            FundingHistoryEntry {
                coin: "BTC".to_owned(),
                funding_rate: rate.parse().expect("valid rate"),
                premium: "0".parse().expect("valid premium"),
                time: TimestampMs::new(time),
            },
            IngestionProvenance {
                source_endpoint_class: SourceEndpointClass::Official,
                ingestion_time: TimestampMs::new(time + 1),
                request_window: RequestWindow::new(TimestampMs::new(time), TimestampMs::new(time))
                    .expect("valid window"),
                software_version: "test".to_owned(),
            },
        )
        .expect("valid record")
    }

    #[test]
    fn apr_reads_latest_parquet_record_and_renders_both_formats() {
        let directory = TestDirectory::new();
        let options = options(directory.0.clone());
        let dataset = SettledFundingDataset::new(&directory.0);
        dataset
            .commit(&[
                record(1_700_000_000_000, "0.00005"),
                record(1_700_003_600_000, "0.0001"),
            ])
            .expect("fixture records commit");

        let view = read_latest_apr(&dataset, &options).expect("APR reads");
        assert_eq!(view.settlement_time_ms, 1_700_003_600_000);
        assert_eq!(view.funding_rate, "0.0001");
        assert_eq!(view.funding_apr, "0.876");
        assert_eq!(view.available_observations, 2);

        let mut human = Vec::new();
        write_apr(&mut human, &view, OutputFormat::Human).expect("human APR writes");
        let human = String::from_utf8(human).expect("UTF-8");
        assert!(human.contains("BTC-PERP · HYPERLIQUID TESTNET"));
        assert!(human.contains("Settlement       2023-11-14T23:13:20.000Z"));
        assert!(human.contains("Hourly funding   +0.01%  (+1 bps)"));
        assert!(human.contains("Simple APR       +87.6%"));
        assert!(human.contains("History          2 hourly observations"));

        let mut json = Vec::new();
        write_apr(&mut json, &view, OutputFormat::Json).expect("JSON APR writes");
        let json: Value = serde_json::from_slice(&json).expect("valid JSON");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["funding_apr"], "0.876");
        assert_eq!(json["settlement_time_ms"], 1_700_003_600_000_i64);
        assert!(json.get("settlement_time_iso").is_none());
    }

    #[test]
    fn apr_reports_empty_stream_as_partial_data() {
        let directory = TestDirectory::new();
        let options = options(directory.0.clone());
        let dataset = SettledFundingDataset::new(&directory.0);

        let error = read_latest_apr(&dataset, &options).expect_err("empty stream must fail");

        assert_eq!(error.category(), ErrorCategory::PartialData);
        assert!(error.to_string().contains("run backfill first"));
    }

    #[test]
    fn operator_scaling_preserves_exact_signs_without_floating_point() {
        let positive: Decimal = "0.0005443322".parse().expect("valid decimal");
        let negative: Decimal = "-0.0005443322".parse().expect("valid decimal");

        assert_eq!(signed_scaled(positive, 100), "+0.05443322");
        assert_eq!(signed_scaled(positive, 10_000), "+5.443322");
        assert_eq!(signed_scaled(negative, 100), "-0.05443322");
        assert_eq!(signed_scaled(Decimal::ZERO, 100), "+0");
    }

    #[test]
    fn history_days_require_complete_contiguous_hourly_observations() {
        let start = 1_700_000_000_000_i64;
        let mut records: Vec<_> = (0..24)
            .map(|hour| record(start + i64::from(hour) * MILLISECONDS_PER_HOUR, "0.0001"))
            .collect();
        assert_eq!(contiguous_history_days(&records), Some(1));

        records[12] = record(start + 13 * MILLISECONDS_PER_HOUR, "0.0001");
        records.sort_by_key(|record| record.identity.settlement_time);
        assert_eq!(contiguous_history_days(&records), None);
    }
}
