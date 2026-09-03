use crate::{
    config::{OutputFormat, SnapshotOptions, TracingMode},
    error::{CliError, ErrorCategory},
};
use hypercarry_core::{
    info::{
        FundingHistoryRequest, InfoClient, InfoClientError, InfoTransport, MetaAndAssetCtxsRequest,
        PredictedFundingsRequest, ReqwestInfoTransport,
    },
    types::{AssetSnapshot, FundingHistory, TimestampMs, VenuePredictionEntry},
};
use serde::Serialize;
use std::{
    io::{self, Write},
    time::{SystemTime, SystemTimeError},
};
use tracing_subscriber::filter::LevelFilter;

const SNAPSHOT_WINDOW_MS: i64 = 4 * 60 * 60 * 1_000;
const JSON_SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
struct MarketSnapshot {
    network: hypercarry_core::info::Network,
    coin: String,
    window_start_time: TimestampMs,
    window_end_time: TimestampMs,
    asset: AssetSnapshot,
    predictions: Vec<VenuePredictionEntry>,
    funding_history: FundingHistory,
}

pub fn configure_tracing(mode: TracingMode) -> Result<(), CliError> {
    tracing_subscriber::fmt()
        .with_max_level(max_level(mode))
        .with_target(mode == TracingMode::Diagnostic)
        .try_init()
        .map_err(|error| {
            CliError::with_boxed_source(
                ErrorCategory::Internal,
                "could not initialize tracing",
                error,
            )
        })
}

fn max_level(mode: TracingMode) -> LevelFilter {
    match mode {
        TracingMode::Quiet => LevelFilter::OFF,
        TracingMode::Normal => LevelFilter::WARN,
        TracingMode::Verbose => LevelFilter::INFO,
        TracingMode::Diagnostic => LevelFilter::DEBUG,
    }
}

pub fn run(options: &SnapshotOptions) -> Result<(), CliError> {
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
    let end_time = current_timestamp_ms()?;
    let snapshot = runtime.block_on(fetch_market_snapshot(&client, &options.coin, end_time))?;

    let stdout = io::stdout();
    write_market_snapshot(&mut stdout.lock(), &snapshot, options.output)
}

pub(crate) fn current_timestamp_ms() -> Result<TimestampMs, CliError> {
    let milliseconds = SystemTime::UNIX_EPOCH
        .elapsed()
        .map_err(clock_error)?
        .as_millis();
    let milliseconds = i64::try_from(milliseconds).map_err(|_| {
        CliError::new(
            ErrorCategory::Internal,
            format!("unix timestamp {milliseconds}ms does not fit in i64"),
        )
    })?;
    Ok(TimestampMs::new(milliseconds))
}

fn clock_error(error: SystemTimeError) -> CliError {
    CliError::with_source(
        ErrorCategory::Internal,
        "could not read the system clock",
        error,
    )
}

async fn fetch_market_snapshot<T: InfoTransport>(
    client: &InfoClient<T>,
    coin: &str,
    end_time: TimestampMs,
) -> Result<MarketSnapshot, CliError> {
    let network = client.network();
    let contexts = client
        .execute(&MetaAndAssetCtxsRequest)
        .await
        .map_err(|error| request_error(network, coin, "metaAndAssetCtxs", error))?;
    let snapshots = contexts.into_snapshots().map_err(|error| {
        CliError::new(
            ErrorCategory::Schema,
            format!(
                "{network} metaAndAssetCtxs returned {} assets but {} contexts",
                error.universe_len, error.ctxs_len
            ),
        )
    })?;
    let asset = snapshots
        .into_iter()
        .find(|snapshot| snapshot.meta.name == coin)
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::PartialData,
                format!("coin {coin:?} is not in the {network} perp universe"),
            )
        })?;
    if asset.meta.is_delisted {
        return Err(CliError::new(
            ErrorCategory::PartialData,
            format!("coin {coin:?} is delisted on {network} and has no live snapshot"),
        ));
    }

    let predictions = client
        .execute(&PredictedFundingsRequest)
        .await
        .map_err(|error| request_error(network, coin, "predictedFundings", error))?
        .into_iter()
        .find(|prediction| prediction.0 == coin)
        .map(|prediction| prediction.1)
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::PartialData,
                format!("{network} predictedFundings returned no entry for coin {coin:?}"),
            )
        })?;

    let start_time = end_time
        .as_i64()
        .checked_sub(SNAPSHOT_WINDOW_MS)
        .map(TimestampMs::new)
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::Internal,
                format!(
                    "snapshot end time {}ms cannot represent a four-hour lookback",
                    end_time.as_i64()
                ),
            )
        })?;
    let funding_history = client
        .execute(&FundingHistoryRequest::new(coin, start_time).with_end_time(end_time))
        .await
        .map_err(|error| request_error(network, coin, "fundingHistory", error))?;

    Ok(MarketSnapshot {
        network,
        coin: coin.to_owned(),
        window_start_time: start_time,
        window_end_time: end_time,
        asset,
        predictions,
        funding_history,
    })
}

fn request_error(
    network: hypercarry_core::info::Network,
    coin: &str,
    request_type: &str,
    error: InfoClientError,
) -> CliError {
    let category = match &error {
        InfoClientError::Transport(_) => ErrorCategory::Network,
        InfoClientError::Decode(_) => ErrorCategory::Schema,
        _ => ErrorCategory::Internal,
    };
    CliError::with_source(
        category,
        format!("{network} {request_type} request failed for coin {coin:?}"),
        error,
    )
}

fn write_market_snapshot(
    output: &mut impl Write,
    snapshot: &MarketSnapshot,
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => write_human(output, snapshot),
        OutputFormat::Json => write_json(output, snapshot),
    }
}

fn write_human(output: &mut impl Write, snapshot: &MarketSnapshot) -> Result<(), CliError> {
    let result = (|| -> io::Result<()> {
        writeln!(output, "network: {}", snapshot.network)?;
        writeln!(output, "coin: {}", snapshot.coin)?;
        writeln!(
            output,
            "funding_window_ms: {}..={}",
            snapshot.window_start_time.as_i64(),
            snapshot.window_end_time.as_i64()
        )?;

        writeln!(output, "\nCONTEXT")?;
        writeln!(output, "{:<22} VALUE", "FIELD")?;
        writeln!(
            output,
            "{:<22} {}",
            "mark_px",
            snapshot.asset.ctx.mark_px.normalize()
        )?;
        writeln!(
            output,
            "{:<22} {}",
            "oracle_px",
            snapshot.asset.ctx.oracle_px.normalize()
        )?;
        writeln!(
            output,
            "{:<22} {}",
            "mid_px",
            snapshot.asset.ctx.mid_px.map_or_else(
                || "unavailable".to_owned(),
                |value| value.normalize().to_string()
            )
        )?;
        writeln!(
            output,
            "{:<22} {}",
            "funding_rate_hourly",
            snapshot.asset.ctx.funding.normalize()
        )?;
        writeln!(
            output,
            "{:<22} {}",
            "open_interest",
            snapshot.asset.ctx.open_interest.normalize()
        )?;

        writeln!(output, "\nPREDICTED FUNDING")?;
        writeln!(
            output,
            "{:<16} {:<16} {:<22} INTERVAL_HOURS",
            "VENUE", "RATE", "NEXT_FUNDING_TIME_MS"
        )?;
        if snapshot.predictions.is_empty() {
            writeln!(output, "none")?;
        }
        for prediction in &snapshot.predictions {
            if let Some(value) = &prediction.1 {
                let interval = value
                    .funding_interval_hours
                    .map_or_else(|| "unknown".to_owned(), |hours| hours.to_string());
                writeln!(
                    output,
                    "{:<16} {:<16} {:<22} {interval}",
                    prediction.0,
                    value.funding_rate.normalize(),
                    value.next_funding_time.as_i64()
                )?;
            } else {
                writeln!(output, "{:<16} unavailable", prediction.0)?;
            }
        }

        writeln!(output, "\nSETTLED FUNDING (LAST 4H)")?;
        writeln!(output, "{:<22} {:<16} PREMIUM", "TIME_MS", "RATE")?;
        if snapshot.funding_history.is_empty() {
            writeln!(output, "none")?;
        }
        for entry in &snapshot.funding_history {
            writeln!(
                output,
                "{:<22} {:<16} {}",
                entry.time.as_i64(),
                entry.funding_rate.normalize(),
                entry.premium.normalize()
            )?;
        }
        Ok(())
    })();
    result.map_err(output_error)
}

fn output_error(error: io::Error) -> CliError {
    CliError::with_source(
        ErrorCategory::Output,
        "could not write snapshot output",
        error,
    )
}

#[derive(Serialize)]
struct JsonSnapshot<'a> {
    schema_version: u32,
    network: hypercarry_core::info::Network,
    coin: &'a str,
    window_start_time_ms: i64,
    window_end_time_ms: i64,
    context: JsonContext,
    predicted_funding: Vec<JsonPrediction<'a>>,
    settled_funding: Vec<JsonFunding<'a>>,
}

#[derive(Serialize)]
struct JsonContext {
    mark_px: String,
    oracle_px: String,
    mid_px: Option<String>,
    funding_rate_hourly: String,
    open_interest: String,
}

#[derive(Serialize)]
struct JsonPrediction<'a> {
    venue: &'a str,
    funding_rate: Option<String>,
    next_funding_time_ms: Option<i64>,
    funding_interval_hours: Option<u32>,
}

#[derive(Serialize)]
struct JsonFunding<'a> {
    coin: &'a str,
    time_ms: i64,
    funding_rate: String,
    premium: String,
}

fn write_json(output: &mut impl Write, snapshot: &MarketSnapshot) -> Result<(), CliError> {
    let value = JsonSnapshot {
        schema_version: JSON_SCHEMA_VERSION,
        network: snapshot.network,
        coin: &snapshot.coin,
        window_start_time_ms: snapshot.window_start_time.as_i64(),
        window_end_time_ms: snapshot.window_end_time.as_i64(),
        context: JsonContext {
            mark_px: snapshot.asset.ctx.mark_px.normalize().to_string(),
            oracle_px: snapshot.asset.ctx.oracle_px.normalize().to_string(),
            mid_px: snapshot
                .asset
                .ctx
                .mid_px
                .map(|value| value.normalize().to_string()),
            funding_rate_hourly: snapshot.asset.ctx.funding.normalize().to_string(),
            open_interest: snapshot.asset.ctx.open_interest.normalize().to_string(),
        },
        predicted_funding: snapshot
            .predictions
            .iter()
            .map(|prediction| JsonPrediction {
                venue: &prediction.0,
                funding_rate: prediction
                    .1
                    .as_ref()
                    .map(|value| value.funding_rate.normalize().to_string()),
                next_funding_time_ms: prediction
                    .1
                    .as_ref()
                    .map(|value| value.next_funding_time.as_i64()),
                funding_interval_hours: prediction
                    .1
                    .as_ref()
                    .and_then(|value| value.funding_interval_hours),
            })
            .collect(),
        settled_funding: snapshot
            .funding_history
            .iter()
            .map(|entry| JsonFunding {
                coin: &entry.coin,
                time_ms: entry.time.as_i64(),
                funding_rate: entry.funding_rate.normalize().to_string(),
                premium: entry.premium.normalize().to_string(),
            })
            .collect(),
    };

    serde_json::to_writer_pretty(&mut *output, &value).map_err(|error| {
        CliError::with_source(
            ErrorCategory::Output,
            "could not serialize snapshot JSON",
            error,
        )
    })?;
    writeln!(output).map_err(output_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercarry_core::info::{BoxError, Network};
    use serde_json::Value;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct FixtureTransport {
        response_override: Option<&'static [u8]>,
        requests: Mutex<Vec<Value>>,
    }

    impl FixtureTransport {
        fn new() -> Self {
            Self {
                response_override: None,
                requests: Mutex::new(Vec::new()),
            }
        }

        fn malformed() -> Self {
            Self {
                response_override: Some(br#"{"unexpected":true}"#),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl InfoTransport for FixtureTransport {
        fn network(&self) -> Network {
            Network::Testnet
        }

        #[allow(clippy::unused_async_trait_impl)]
        async fn post(&self, request_body: &[u8]) -> Result<Vec<u8>, BoxError> {
            let body: Value = serde_json::from_slice(request_body)?;
            self.requests
                .lock()
                .expect("request lock poisoned")
                .push(body.clone());
            if let Some(response) = self.response_override {
                return Ok(response.to_vec());
            }
            let response = match body.get("type").and_then(Value::as_str) {
                Some("fundingHistory") => {
                    include_bytes!("../../hypercarry-core/tests/fixtures/funding_history.json")
                        .as_slice()
                }
                Some("metaAndAssetCtxs") => {
                    include_bytes!("../../hypercarry-core/tests/fixtures/meta_and_asset_ctxs.json")
                        .as_slice()
                }
                Some("predictedFundings") => {
                    include_bytes!("../../hypercarry-core/tests/fixtures/predicted_fundings.json")
                        .as_slice()
                }
                request_type => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unexpected request type: {request_type:?}"),
                    )
                    .into());
                }
            };
            Ok(response.to_vec())
        }
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime builds")
            .block_on(future)
    }

    fn fixture_snapshot(client: &InfoClient<FixtureTransport>) -> MarketSnapshot {
        block_on(fetch_market_snapshot(
            client,
            "BTC",
            TimestampMs::new(1_700_000_000_000),
        ))
        .expect("fixture snapshot succeeds")
    }

    #[test]
    fn snapshot_fetches_all_three_sources_and_bounds_the_window() {
        let client = InfoClient::new(FixtureTransport::new());
        let snapshot = fixture_snapshot(&client);

        assert_eq!(snapshot.network, Network::Testnet);
        assert_eq!(snapshot.coin, "BTC");
        assert_eq!(snapshot.asset.meta.name, "BTC");
        assert!(!snapshot.predictions.is_empty());
        assert_eq!(snapshot.funding_history.len(), 3);

        let requests = client
            .transport()
            .requests
            .lock()
            .expect("request lock poisoned");
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0]["type"], "metaAndAssetCtxs");
        assert_eq!(requests[1]["type"], "predictedFundings");
        assert_eq!(requests[2]["type"], "fundingHistory");
        assert_eq!(requests[2]["coin"], "BTC");
        assert_eq!(
            requests[2]["startTime"],
            snapshot.window_start_time.as_i64()
        );
        assert_eq!(requests[2]["endTime"], snapshot.window_end_time.as_i64());
    }

    #[test]
    fn human_and_json_output_include_network_units_and_stable_schema() {
        let client = InfoClient::new(FixtureTransport::new());
        let snapshot = fixture_snapshot(&client);

        let mut human = Vec::new();
        write_market_snapshot(&mut human, &snapshot, OutputFormat::Human)
            .expect("human snapshot renders");
        let human = String::from_utf8(human).expect("human output is UTF-8");
        assert!(human.contains("network: testnet"));
        assert!(human.contains("NEXT_FUNDING_TIME_MS"));
        assert!(human.contains("SETTLED FUNDING (LAST 4H)"));

        let mut json = Vec::new();
        write_market_snapshot(&mut json, &snapshot, OutputFormat::Json)
            .expect("JSON snapshot renders");
        let json: Value = serde_json::from_slice(&json).expect("output is JSON");
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["network"], "testnet");
        assert_eq!(json["coin"], "BTC");
        assert!(json["window_start_time_ms"].is_i64());
        assert!(json["context"]["mark_px"].is_string());
        assert!(json["predicted_funding"].is_array());
        assert!(json["settled_funding"][0]["time_ms"].is_i64());
    }

    #[test]
    fn unknown_coin_stops_early_as_partial_data() {
        let client = InfoClient::new(FixtureTransport::new());
        let error = block_on(fetch_market_snapshot(
            &client,
            "NOT_A_COIN",
            TimestampMs::new(1_700_000_000_000),
        ))
        .expect_err("unknown coin must fail");

        assert_eq!(error.category(), ErrorCategory::PartialData);
        assert_eq!(error.exit_code(), 14);
        assert_eq!(
            client
                .transport()
                .requests
                .lock()
                .expect("request lock poisoned")
                .len(),
            1
        );
    }

    #[test]
    fn malformed_response_is_a_schema_error() {
        let client = InfoClient::new(FixtureTransport::malformed());
        let error = block_on(fetch_market_snapshot(
            &client,
            "BTC",
            TimestampMs::new(1_700_000_000_000),
        ))
        .expect_err("malformed response must fail");

        assert_eq!(error.category(), ErrorCategory::Schema);
        assert_eq!(error.exit_code(), 12);
        assert!(error.to_string().contains("metaAndAssetCtxs"));
    }

    #[test]
    fn tracing_modes_map_to_bounded_levels() {
        assert_eq!(max_level(TracingMode::Quiet), LevelFilter::OFF);
        assert_eq!(max_level(TracingMode::Normal), LevelFilter::WARN);
        assert_eq!(max_level(TracingMode::Verbose), LevelFilter::INFO);
        assert_eq!(max_level(TracingMode::Diagnostic), LevelFilter::DEBUG);
    }
}
