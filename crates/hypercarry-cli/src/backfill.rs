use crate::{
    config::{BackfillOptions, OutputFormat},
    error::{CliError, ErrorCategory},
    snapshot::current_timestamp_ms,
};
use hypercarry_core::{
    info::{
        FundingHistoryPaginationError, InfoClient, InfoClientError, InfoTransport,
        ReqwestInfoTransport,
    },
    types::{FundingHistory, TimestampMs},
};
use hypercarry_storage::{
    dataset::{CommitReport, SettledFundingDataset},
    settled_funding::{
        IngestionProvenance, RequestWindow, SettledFundingRecord, SourceEndpointClass,
    },
};
use serde::Serialize;
use std::{
    future::Future,
    io::{self, Write},
};

const MILLISECONDS_PER_DAY: i64 = 24 * 60 * 60 * 1_000;
const VENUE: &str = "hyperliquid";
const JSON_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
struct BackfillSummary<'a> {
    schema_version: u32,
    network: hypercarry_core::info::Network,
    coin: &'a str,
    venue: &'static str,
    requested_start_time_ms: i64,
    effective_start_time_ms: i64,
    end_time_ms: i64,
    fetched_rows: usize,
    rows_added: usize,
    duplicates_collapsed: usize,
    partitions_written: usize,
    checkpoints_written: usize,
}

pub fn run(options: &BackfillOptions) -> Result<(), CliError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            CliError::with_source(
                ErrorCategory::Internal,
                "could not initialize the async runtime",
                error,
            )
        })?;
    let transport = ReqwestInfoTransport::for_network(options.network).map_err(|error| {
        CliError::with_source(
            ErrorCategory::Network,
            format!("could not initialize the {} HTTP client", options.network),
            error,
        )
    })?;
    let client = InfoClient::new(transport);
    let dataset = SettledFundingDataset::new(&options.dataset);
    let end_time = current_timestamp_ms()?;
    let stderr = io::stderr();
    let summary = runtime.block_on(execute(
        options,
        &client,
        &dataset,
        end_time,
        async {
            tokio::signal::ctrl_c().await.map_err(|error| {
                CliError::with_source(
                    ErrorCategory::Internal,
                    "could not install the Ctrl-C handler",
                    error,
                )
            })
        },
        &mut stderr.lock(),
    ))?;
    let stdout = io::stdout();
    write_summary(&mut stdout.lock(), &summary, options.output)
}

async fn execute<'a, T, C>(
    options: &'a BackfillOptions,
    client: &InfoClient<T>,
    dataset: &SettledFundingDataset,
    end_time: TimestampMs,
    cancellation: C,
    progress: &mut impl Write,
) -> Result<BackfillSummary<'a>, CliError>
where
    T: InfoTransport,
    C: Future<Output = Result<(), CliError>>,
{
    let (requested_start, effective_start) = backfill_window(options, dataset, end_time)?;
    progress_line(
        progress,
        format_args!(
            "backfill: fetching {} {} from {}..={}ms",
            options.network,
            options.coin,
            effective_start.as_i64(),
            end_time.as_i64()
        ),
    )?;

    tokio::pin!(cancellation);
    let history = tokio::select! {
        biased;
        cancellation_result = &mut cancellation => {
            cancellation_result?;
            return Err(CliError::new(
                ErrorCategory::Cancelled,
                "backfill cancelled before any storage commit; rerun the same command to resume from the last durable checkpoint",
            ));
        }
        result = client.funding_history_range(&options.coin, effective_start, end_time) => {
            result.map_err(|error| pagination_error(options, error))?
        }
    };
    progress_line(
        progress,
        format_args!("backfill: fetched {} settled funding rows", history.len()),
    )?;
    let report = normalize_and_commit(options, dataset, history, effective_start, end_time)?;
    progress_line(
        progress,
        format_args!(
            "backfill: committed {} new rows across {} partitions; {} overlaps collapsed",
            report.rows_added, report.partitions_written, report.duplicates_collapsed
        ),
    )?;

    Ok(summary(
        options,
        requested_start,
        effective_start,
        end_time,
        report,
    ))
}

fn backfill_window(
    options: &BackfillOptions,
    dataset: &SettledFundingDataset,
    end_time: TimestampMs,
) -> Result<(TimestampMs, TimestampMs), CliError> {
    let lookback_ms = i64::from(options.days)
        .checked_mul(MILLISECONDS_PER_DAY)
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::Configuration,
                format!("backfill window of {} days is too large", options.days),
            )
        })?;
    let requested_start = end_time
        .as_i64()
        .checked_sub(lookback_ms)
        .map(TimestampMs::new)
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::Configuration,
                format!(
                    "backfill window of {} days is outside timestamp range",
                    options.days
                ),
            )
        })?;
    let checkpoint = dataset
        .resume_checkpoint(options.network, VENUE, &options.coin)
        .map_err(storage_error)?;
    let effective_start = checkpoint.as_ref().map_or(requested_start, |checkpoint| {
        checkpoint.last_settlement_time_ms.max(requested_start)
    });
    if effective_start > end_time {
        return Err(CliError::new(
            ErrorCategory::Storage,
            format!(
                "checkpoint {}ms is later than backfill end {}ms for {}/{}/{}",
                effective_start.as_i64(),
                end_time.as_i64(),
                options.network,
                VENUE,
                options.coin
            ),
        ));
    }
    Ok((requested_start, effective_start))
}

fn normalize_and_commit(
    options: &BackfillOptions,
    dataset: &SettledFundingDataset,
    history: FundingHistory,
    effective_start: TimestampMs,
    end_time: TimestampMs,
) -> Result<CommitReport, CliError> {
    let request_window = RequestWindow::new(effective_start, end_time).map_err(|error| {
        CliError::with_source(
            ErrorCategory::Storage,
            "could not construct backfill provenance window",
            error,
        )
    })?;
    let provenance = IngestionProvenance::for_current_build(
        SourceEndpointClass::Official,
        current_timestamp_ms()?,
        request_window,
    );
    let records = history
        .into_iter()
        .map(|entry| {
            SettledFundingRecord::from_history_entry(
                options.network,
                VENUE,
                entry,
                provenance.clone(),
            )
            .map_err(|error| {
                CliError::with_source(
                    ErrorCategory::Schema,
                    "funding-history row violates the settled-funding contract",
                    error,
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    dataset.commit(&records).map_err(storage_error)
}

fn summary(
    options: &BackfillOptions,
    requested_start: TimestampMs,
    effective_start: TimestampMs,
    end_time: TimestampMs,
    report: CommitReport,
) -> BackfillSummary<'_> {
    BackfillSummary {
        schema_version: JSON_SCHEMA_VERSION,
        network: options.network,
        coin: &options.coin,
        venue: VENUE,
        requested_start_time_ms: requested_start.as_i64(),
        effective_start_time_ms: effective_start.as_i64(),
        end_time_ms: end_time.as_i64(),
        fetched_rows: report.input_rows,
        rows_added: report.rows_added,
        duplicates_collapsed: report.duplicates_collapsed,
        partitions_written: report.partitions_written,
        checkpoints_written: report.checkpoints_written,
    }
}

fn pagination_error(options: &BackfillOptions, error: FundingHistoryPaginationError) -> CliError {
    let category = match &error {
        FundingHistoryPaginationError::Request(InfoClientError::Transport(_)) => {
            ErrorCategory::Network
        }
        FundingHistoryPaginationError::Request(InfoClientError::Decode(_)) => ErrorCategory::Schema,
        FundingHistoryPaginationError::Request(_) => ErrorCategory::Internal,
        _ => ErrorCategory::Schema,
    };
    CliError::with_source(
        category,
        format!(
            "{} funding-history backfill failed for coin {:?}",
            options.network, options.coin
        ),
        error,
    )
}

fn storage_error(error: hypercarry_storage::dataset::DatasetError) -> CliError {
    CliError::with_source(
        ErrorCategory::Storage,
        "settled-funding dataset operation failed",
        error,
    )
}

fn progress_line(
    output: &mut impl Write,
    arguments: std::fmt::Arguments<'_>,
) -> Result<(), CliError> {
    writeln!(output, "{arguments}").map_err(|error| {
        CliError::with_source(
            ErrorCategory::Output,
            "could not write backfill progress",
            error,
        )
    })
}

fn write_summary(
    output: &mut impl Write,
    summary: &BackfillSummary<'_>,
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => writeln!(
            output,
            "backfill complete: network={} coin={} fetched={} added={} duplicates={} partitions={} checkpoints={}",
            summary.network,
            summary.coin,
            summary.fetched_rows,
            summary.rows_added,
            summary.duplicates_collapsed,
            summary.partitions_written,
            summary.checkpoints_written
        )
        .map_err(output_error),
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *output, summary).map_err(|error| {
                CliError::with_source(
                    ErrorCategory::Output,
                    "could not serialize backfill JSON",
                    error,
                )
            })?;
            writeln!(output).map_err(output_error)
        }
    }
}

fn output_error(error: io::Error) -> CliError {
    CliError::with_source(
        ErrorCategory::Output,
        "could not write backfill output",
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercarry_core::info::{BoxError, Network};
    use serde_json::{Value, json};
    use std::{
        fs,
        path::PathBuf,
        sync::{
            Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "hypercarry-backfill-test-{}-{sequence}",
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

    #[derive(Debug)]
    struct FixtureTransport {
        times: Vec<i64>,
        requests: Mutex<Vec<Value>>,
    }

    impl InfoTransport for FixtureTransport {
        fn network(&self) -> Network {
            Network::Testnet
        }

        #[allow(clippy::unused_async_trait_impl)]
        async fn post(&self, request_body: &[u8]) -> Result<Vec<u8>, BoxError> {
            let request: Value = serde_json::from_slice(request_body)?;
            let start = request["startTime"].as_i64().expect("request has start");
            let end = request["endTime"].as_i64().expect("request has end");
            self.requests
                .lock()
                .expect("request lock poisoned")
                .push(request);
            let rows: Vec<_> = self
                .times
                .iter()
                .copied()
                .filter(|time| start <= *time && *time <= end)
                .map(|time| {
                    json!({
                        "coin": "BTC",
                        "fundingRate": "0.0001",
                        "premium": "0.00002",
                        "time": time
                    })
                })
                .collect();
            Ok(serde_json::to_vec(&rows)?)
        }
    }

    fn options(path: PathBuf) -> BackfillOptions {
        BackfillOptions {
            network: Network::Testnet,
            coin: "BTC".to_owned(),
            days: 1,
            dataset: path,
            output: OutputFormat::Json,
            tracing: crate::config::TracingMode::Quiet,
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds")
            .block_on(future)
    }

    #[test]
    fn fixture_backfill_writes_and_resumes_inclusively_with_progress() {
        let directory = TestDirectory::new();
        let options = options(directory.0.clone());
        let end = TimestampMs::new(1_700_000_000_000);
        let times = vec![end.as_i64() - 7_200_000, end.as_i64() - 3_600_000];
        let client = InfoClient::new(FixtureTransport {
            times: times.clone(),
            requests: Mutex::new(Vec::new()),
        });
        let dataset = SettledFundingDataset::new(&directory.0);
        let mut progress = Vec::new();

        let first = block_on(execute(
            &options,
            &client,
            &dataset,
            end,
            std::future::pending(),
            &mut progress,
        ))
        .expect("first backfill succeeds");
        assert_eq!(first.fetched_rows, 2);
        assert_eq!(first.rows_added, 2);
        assert_eq!(first.checkpoints_written, 1);

        let second = block_on(execute(
            &options,
            &client,
            &dataset,
            end,
            std::future::pending(),
            &mut progress,
        ))
        .expect("resume backfill succeeds");
        assert_eq!(second.effective_start_time_ms, times[1]);
        assert_eq!(second.fetched_rows, 1);
        assert_eq!(second.rows_added, 0);
        assert_eq!(second.duplicates_collapsed, 1);
        assert_eq!(second.checkpoints_written, 0);
        assert!(
            String::from_utf8(progress)
                .expect("progress is UTF-8")
                .contains("committed")
        );
    }

    #[test]
    fn cancellation_wins_before_network_or_storage_mutation() {
        let directory = TestDirectory::new();
        let options = options(directory.0.clone());
        let client = InfoClient::new(FixtureTransport {
            times: Vec::new(),
            requests: Mutex::new(Vec::new()),
        });
        let dataset = SettledFundingDataset::new(&directory.0);
        let mut progress = Vec::new();

        let error = block_on(execute(
            &options,
            &client,
            &dataset,
            TimestampMs::new(1_700_000_000_000),
            std::future::ready(Ok(())),
            &mut progress,
        ))
        .expect_err("ready cancellation stops the backfill");

        assert_eq!(error.category(), ErrorCategory::Cancelled);
        assert!(error.to_string().contains("rerun the same command"));
        assert!(client.transport().requests.lock().expect("lock").is_empty());
        assert!(
            dataset
                .stream_records(Network::Testnet, VENUE, "BTC")
                .expect("stream read succeeds")
                .is_empty()
        );
    }

    #[test]
    fn summary_supports_stable_human_and_json_output() {
        let directory = TestDirectory::new();
        let options = options(directory.0.clone());
        let summary = summary(
            &options,
            TimestampMs::new(1),
            TimestampMs::new(2),
            TimestampMs::new(3),
            CommitReport {
                input_rows: 4,
                rows_added: 3,
                duplicates_collapsed: 1,
                partitions_written: 2,
                checkpoints_written: 1,
            },
        );
        let mut json_output = Vec::new();
        write_summary(&mut json_output, &summary, OutputFormat::Json).expect("JSON writes");
        let json: Value = serde_json::from_slice(&json_output).expect("valid JSON");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["rows_added"], 3);

        let mut human_output = Vec::new();
        write_summary(&mut human_output, &summary, OutputFormat::Human).expect("human writes");
        assert!(
            String::from_utf8(human_output)
                .expect("UTF-8")
                .contains("added=3")
        );
    }
}
