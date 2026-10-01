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
    /// Unbroken hourly run ending at the newest observation.
    ///
    /// Exposed to automation because a monitor needs to assert that recent
    /// history is complete, which `available_observations` alone cannot show.
    contiguous_history_hours: usize,
    #[serde(skip)]
    settlement_time_iso: String,
    #[serde(skip)]
    funding_rate_percent: String,
    #[serde(skip)]
    funding_rate_bps: String,
    #[serde(skip)]
    funding_apr_percent: String,
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
        contiguous_history_hours: contiguous_history_hours(&records),
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

/// Hours of unbroken hourly history ending at the newest observation.
///
/// Measured backwards from the newest record, because that is the run a current
/// decision rests on: a gap in old history says nothing about whether the last
/// week is complete, and a whole-dataset check would erase the recent run
/// forever the first time one settlement went missing. Counted in hours rather
/// than whole days so a continuously recording host reports a rising number
/// every hour instead of only at exact day boundaries.
fn contiguous_history_hours(records: &[SettledFundingRecord]) -> usize {
    let broken_at = records
        .windows(2)
        .rposition(|window| {
            let elapsed = window[1].identity.settlement_time.as_i64()
                - window[0].identity.settlement_time.as_i64();
            (elapsed - MILLISECONDS_PER_HOUR).abs() > MAX_SETTLEMENT_JITTER_MS
        })
        // `rposition` indexes the pair; the run starts at its second element.
        .map_or(0, |index| index + 1);
    records.len() - broken_at
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
                writeln!(
                    output,
                    "{:<17}{}",
                    "History",
                    history_summary(view.available_observations, view.contiguous_history_hours)
                )
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

/// How much history there is, and how much of it is unbroken.
///
/// The contiguous run is stated even when it is shorter than the dataset: a
/// reader shown only a total cannot tell a complete week from a month with
/// holes in it, and the two support very different conclusions.
fn history_summary(observations: usize, contiguous: usize) -> String {
    if observations == 1 {
        return "1 hourly observation".to_owned();
    }
    let span = match contiguous / 24 {
        0 => String::new(),
        1 => " (1 day)".to_owned(),
        days => format!(" ({days} days)"),
    };
    if observations == contiguous {
        format!("{observations} hourly observations, all contiguous{span}")
    } else {
        format!("{observations} hourly observations, latest {contiguous} contiguous{span}")
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
    fn contiguous_history_is_the_unbroken_run_ending_at_the_newest_observation() {
        let start = 1_700_000_000_000_i64;
        let hourly = |count: i64| -> Vec<_> {
            (0..count)
                .map(|hour| record(start + hour * MILLISECONDS_PER_HOUR, "0.0001"))
                .collect()
        };

        assert_eq!(contiguous_history_hours(&hourly(24)), 24);
        // A run that is not a whole number of days still reports its length;
        // the old check reported nothing except at exact day boundaries.
        assert_eq!(contiguous_history_hours(&hourly(25)), 25);
        assert_eq!(contiguous_history_hours(&hourly(1)), 1);
        assert_eq!(contiguous_history_hours(&[]), 0);

        // An old gap does not erase the recent run a decision rests on.
        let mut gapped = hourly(24);
        gapped.remove(3);
        assert_eq!(contiguous_history_hours(&gapped), 20);

        // A gap at the newest end leaves only what follows it.
        let mut recent_gap = hourly(24);
        recent_gap.remove(22);
        assert_eq!(contiguous_history_hours(&recent_gap), 1);
    }

    #[test]
    fn history_summary_distinguishes_a_complete_run_from_a_dataset_with_holes() {
        assert_eq!(history_summary(1, 1), "1 hourly observation");
        assert_eq!(
            history_summary(168, 168),
            "168 hourly observations, all contiguous (7 days)"
        );
        assert_eq!(
            history_summary(24, 24),
            "24 hourly observations, all contiguous (1 day)"
        );
        assert_eq!(
            history_summary(200, 72),
            "200 hourly observations, latest 72 contiguous (3 days)"
        );
        assert_eq!(
            history_summary(10, 4),
            "10 hourly observations, latest 4 contiguous"
        );
    }
}
