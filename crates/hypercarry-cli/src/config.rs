use crate::error::{CliError, ErrorCategory};
use clap::ValueEnum;
use hypercarry_core::info::Network;
use rust_decimal::Decimal;
use serde::Deserialize;
use std::{ffi::OsString, fs, path::PathBuf, str::FromStr};

const CONFIG_ENV: &str = "HYPERCARRY_CONFIG";
const NETWORK_ENV: &str = "HYPERCARRY_NETWORK";
const COIN_ENV: &str = "HYPERCARRY_COIN";
const COINS_ENV: &str = "HYPERCARRY_COINS";
const OUTPUT_ENV: &str = "HYPERCARRY_OUTPUT";
const TRACING_ENV: &str = "HYPERCARRY_TRACING";
const DATASET_ENV: &str = "HYPERCARRY_DATASET";
const DAYS_ENV: &str = "HYPERCARRY_DAYS";
const QUEUE_CAPACITY_ENV: &str = "HYPERCARRY_QUEUE_CAPACITY";
const CAPTURE_ENV: &str = "HYPERCARRY_CAPTURE";
const SETTLEMENT_MS_ENV: &str = "HYPERCARRY_SETTLEMENT_MS";
const AS_OF_MS_ENV: &str = "HYPERCARRY_AS_OF_MS";
const OFFICIAL_RATE_ENV: &str = "HYPERCARRY_OFFICIAL_RATE";
const OFFICIAL_OBSERVED_AT_MS_ENV: &str = "HYPERCARRY_OFFICIAL_OBSERVED_AT_MS";
const REFRESH_MS_ENV: &str = "HYPERCARRY_REFRESH_MS";
const COLOR_ENV: &str = "HYPERCARRY_COLOR";
const DEFAULT_BACKFILL_DAYS: u32 = 30;
const DEFAULT_DATASET_PATH: &str = "data";
const DEFAULT_TUI_REFRESH_MS: u64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TracingMode {
    Quiet,
    Normal,
    Verbose,
    Diagnostic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Default)]
pub struct SnapshotOverrides {
    pub network: Option<Network>,
    pub coin: Option<String>,
    pub output: Option<OutputFormat>,
    pub tracing: Option<TracingMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotOptions {
    pub network: Network,
    pub coin: String,
    pub output: OutputFormat,
    pub tracing: TracingMode,
}

#[derive(Debug, Default)]
pub struct BackfillOverrides {
    pub network: Option<Network>,
    pub coin: Option<String>,
    pub days: Option<u32>,
    pub dataset: Option<PathBuf>,
    pub output: Option<OutputFormat>,
    pub tracing: Option<TracingMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillOptions {
    pub network: Network,
    pub coin: String,
    pub days: u32,
    pub dataset: PathBuf,
    pub output: OutputFormat,
    pub tracing: TracingMode,
}

#[derive(Debug, Default)]
pub struct AprOverrides {
    pub network: Option<Network>,
    pub coin: Option<String>,
    pub dataset: Option<PathBuf>,
    pub output: Option<OutputFormat>,
    pub tracing: Option<TracingMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprOptions {
    pub network: Network,
    pub coin: String,
    pub dataset: PathBuf,
    pub output: OutputFormat,
    pub tracing: TracingMode,
}

#[derive(Debug, Default)]
pub struct RecordOverrides {
    pub network: Option<Network>,
    pub coins: Option<Vec<String>>,
    pub dataset: Option<PathBuf>,
    pub queue_capacity: Option<usize>,
    pub output: Option<OutputFormat>,
    pub tracing: Option<TracingMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordOptions {
    pub network: Network,
    pub coins: Vec<String>,
    pub dataset: PathBuf,
    pub queue_capacity: usize,
    pub output: OutputFormat,
    pub tracing: TracingMode,
}

#[derive(Debug, Default)]
pub struct PredictOverrides {
    pub network: Option<Network>,
    pub coin: Option<String>,
    pub capture: Option<PathBuf>,
    pub settlement_ms: Option<i64>,
    pub as_of_ms: Option<i64>,
    pub dataset: Option<PathBuf>,
    pub official_rate: Option<Decimal>,
    pub official_observed_at_ms: Option<i64>,
    pub output: Option<OutputFormat>,
    pub tracing: Option<TracingMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictOptions {
    pub network: Network,
    pub coin: String,
    pub capture: PathBuf,
    pub settlement_ms: i64,
    pub as_of_ms: i64,
    pub dataset: PathBuf,
    pub official_rate: Option<Decimal>,
    pub official_observed_at_ms: Option<i64>,
    pub output: OutputFormat,
    pub tracing: TracingMode,
}

#[derive(Debug, Default)]
pub struct TuiOverrides {
    pub network: Option<Network>,
    pub coin: Option<String>,
    pub capture: Option<PathBuf>,
    pub dataset: Option<PathBuf>,
    pub refresh_ms: Option<u64>,
    pub color: Option<ColorMode>,
    pub tracing: Option<TracingMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiOptions {
    pub network: Network,
    pub coin: String,
    pub capture: PathBuf,
    pub dataset: PathBuf,
    pub refresh_ms: u64,
    pub color: ColorMode,
    pub tracing: TracingMode,
}

pub trait Environment {
    fn get(&self, name: &str) -> Option<OsString>;
}

pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    snapshot: SnapshotFileConfig,
    backfill: BackfillFileConfig,
    apr: AprFileConfig,
    record: RecordFileConfig,
    predict: PredictFileConfig,
    tui: TuiFileConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SnapshotFileConfig {
    network: Option<Network>,
    coin: Option<String>,
    output: Option<OutputFormat>,
    tracing: Option<TracingMode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BackfillFileConfig {
    network: Option<Network>,
    coin: Option<String>,
    days: Option<u32>,
    dataset: Option<PathBuf>,
    output: Option<OutputFormat>,
    tracing: Option<TracingMode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AprFileConfig {
    network: Option<Network>,
    coin: Option<String>,
    dataset: Option<PathBuf>,
    output: Option<OutputFormat>,
    tracing: Option<TracingMode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RecordFileConfig {
    network: Option<Network>,
    coins: Option<Vec<String>>,
    dataset: Option<PathBuf>,
    queue_capacity: Option<usize>,
    output: Option<OutputFormat>,
    tracing: Option<TracingMode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PredictFileConfig {
    network: Option<Network>,
    coin: Option<String>,
    capture: Option<PathBuf>,
    settlement_ms: Option<i64>,
    as_of_ms: Option<i64>,
    dataset: Option<PathBuf>,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    official_rate: Option<Decimal>,
    official_observed_at_ms: Option<i64>,
    output: Option<OutputFormat>,
    tracing: Option<TracingMode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TuiFileConfig {
    network: Option<Network>,
    coin: Option<String>,
    capture: Option<PathBuf>,
    dataset: Option<PathBuf>,
    refresh_ms: Option<u64>,
    color: Option<ColorMode>,
    tracing: Option<TracingMode>,
}

pub fn resolve_snapshot(
    cli_config_path: Option<PathBuf>,
    cli: SnapshotOverrides,
    environment: &impl Environment,
) -> Result<SnapshotOptions, CliError> {
    let config_path = cli_config_path.or_else(|| environment.get(CONFIG_ENV).map(PathBuf::from));
    let file = load_file_config(config_path.as_ref())?;
    let environment = read_environment(environment)?;
    resolve_layers(cli, environment, file.snapshot)
}

pub fn resolve_backfill(
    cli_config_path: Option<PathBuf>,
    cli: BackfillOverrides,
    environment: &impl Environment,
) -> Result<BackfillOptions, CliError> {
    let config_path = cli_config_path.or_else(|| environment.get(CONFIG_ENV).map(PathBuf::from));
    let file = load_file_config(config_path.as_ref())?;
    let environment = read_backfill_environment(environment)?;
    resolve_backfill_layers(cli, environment, file.backfill)
}

pub fn resolve_apr(
    cli_config_path: Option<PathBuf>,
    cli: AprOverrides,
    environment: &impl Environment,
) -> Result<AprOptions, CliError> {
    let config_path = cli_config_path.or_else(|| environment.get(CONFIG_ENV).map(PathBuf::from));
    let file = load_file_config(config_path.as_ref())?;
    let environment = read_apr_environment(environment)?;
    resolve_apr_layers(cli, environment, file.apr)
}

pub fn resolve_record(
    cli_config_path: Option<PathBuf>,
    cli: RecordOverrides,
    environment: &impl Environment,
) -> Result<RecordOptions, CliError> {
    let config_path = cli_config_path.or_else(|| environment.get(CONFIG_ENV).map(PathBuf::from));
    let file = load_file_config(config_path.as_ref())?;
    let environment = read_record_environment(environment)?;
    resolve_record_layers(cli, environment, file.record)
}

pub fn resolve_predict(
    cli_config_path: Option<PathBuf>,
    cli: PredictOverrides,
    environment: &impl Environment,
) -> Result<PredictOptions, CliError> {
    let config_path = cli_config_path.or_else(|| environment.get(CONFIG_ENV).map(PathBuf::from));
    let file = load_file_config(config_path.as_ref())?;
    let environment = read_predict_environment(environment)?;
    resolve_predict_layers(cli, environment, file.predict)
}

pub fn resolve_tui(
    cli_config_path: Option<PathBuf>,
    cli: TuiOverrides,
    environment: &impl Environment,
) -> Result<TuiOptions, CliError> {
    let config_path = cli_config_path.or_else(|| environment.get(CONFIG_ENV).map(PathBuf::from));
    let file = load_file_config(config_path.as_ref())?;
    let environment = read_tui_environment(environment)?;
    resolve_tui_layers(cli, environment, file.tui)
}

fn load_file_config(path: Option<&PathBuf>) -> Result<FileConfig, CliError> {
    let Some(path) = path else {
        return Ok(FileConfig::default());
    };
    let contents = fs::read_to_string(path).map_err(|error| {
        CliError::with_source(
            ErrorCategory::Configuration,
            format!("could not read JSON config file {}", path.display()),
            error,
        )
    })?;
    parse_file_config(&contents).map_err(|error| {
        CliError::with_source(
            ErrorCategory::Configuration,
            format!("could not parse JSON config file {}", path.display()),
            error,
        )
    })
}

fn parse_file_config(contents: &str) -> Result<FileConfig, serde_json::Error> {
    serde_json::from_str(contents)
}

fn read_environment(environment: &impl Environment) -> Result<SnapshotOverrides, CliError> {
    Ok(SnapshotOverrides {
        network: env_string(environment, NETWORK_ENV)?
            .map(|value| {
                Network::from_str(&value).map_err(|error| {
                    CliError::with_source(
                        ErrorCategory::Configuration,
                        format!("invalid {NETWORK_ENV} value {value:?}"),
                        error,
                    )
                })
            })
            .transpose()?,
        coin: env_string(environment, COIN_ENV)?,
        output: env_string(environment, OUTPUT_ENV)?
            .map(|value| parse_output(&value))
            .transpose()?,
        tracing: env_string(environment, TRACING_ENV)?
            .map(|value| parse_tracing(&value))
            .transpose()?,
    })
}

fn read_backfill_environment(
    environment: &impl Environment,
) -> Result<BackfillOverrides, CliError> {
    Ok(BackfillOverrides {
        network: read_network_environment(environment)?,
        coin: env_string(environment, COIN_ENV)?,
        days: env_string(environment, DAYS_ENV)?
            .map(|value| {
                value.parse::<u32>().map_err(|error| {
                    CliError::with_source(
                        ErrorCategory::Configuration,
                        format!("invalid {DAYS_ENV} value {value:?}; expected a positive integer"),
                        error,
                    )
                })
            })
            .transpose()?,
        dataset: env_string(environment, DATASET_ENV)?.map(PathBuf::from),
        output: env_string(environment, OUTPUT_ENV)?
            .map(|value| parse_output(&value))
            .transpose()?,
        tracing: env_string(environment, TRACING_ENV)?
            .map(|value| parse_tracing(&value))
            .transpose()?,
    })
}

fn read_apr_environment(environment: &impl Environment) -> Result<AprOverrides, CliError> {
    Ok(AprOverrides {
        network: read_network_environment(environment)?,
        coin: env_string(environment, COIN_ENV)?,
        dataset: env_string(environment, DATASET_ENV)?.map(PathBuf::from),
        output: env_string(environment, OUTPUT_ENV)?
            .map(|value| parse_output(&value))
            .transpose()?,
        tracing: env_string(environment, TRACING_ENV)?
            .map(|value| parse_tracing(&value))
            .transpose()?,
    })
}

fn read_record_environment(environment: &impl Environment) -> Result<RecordOverrides, CliError> {
    Ok(RecordOverrides {
        network: read_network_environment(environment)?,
        coins: env_string(environment, COINS_ENV)?.map(|value| {
            value
                .split(',')
                .map(|coin| coin.trim().to_owned())
                .collect()
        }),
        dataset: env_string(environment, DATASET_ENV)?.map(PathBuf::from),
        queue_capacity: env_string(environment, QUEUE_CAPACITY_ENV)?
            .map(|value| {
                value.parse::<usize>().map_err(|error| {
                    CliError::with_source(
                        ErrorCategory::Configuration,
                        format!(
                            "invalid {QUEUE_CAPACITY_ENV} value {value:?}; expected a positive integer"
                        ),
                        error,
                    )
                })
            })
            .transpose()?,
        output: env_string(environment, OUTPUT_ENV)?
            .map(|value| parse_output(&value))
            .transpose()?,
        tracing: env_string(environment, TRACING_ENV)?
            .map(|value| parse_tracing(&value))
            .transpose()?,
    })
}

fn read_predict_environment(environment: &impl Environment) -> Result<PredictOverrides, CliError> {
    Ok(PredictOverrides {
        network: read_network_environment(environment)?,
        coin: env_string(environment, COIN_ENV)?,
        capture: env_string(environment, CAPTURE_ENV)?.map(PathBuf::from),
        settlement_ms: read_i64_environment(environment, SETTLEMENT_MS_ENV)?,
        as_of_ms: read_i64_environment(environment, AS_OF_MS_ENV)?,
        dataset: env_string(environment, DATASET_ENV)?.map(PathBuf::from),
        official_rate: env_string(environment, OFFICIAL_RATE_ENV)?
            .map(|value| {
                value.parse::<Decimal>().map_err(|error| {
                    CliError::with_source(
                        ErrorCategory::Configuration,
                        format!("invalid {OFFICIAL_RATE_ENV} value {value:?}; expected a decimal"),
                        error,
                    )
                })
            })
            .transpose()?,
        official_observed_at_ms: read_i64_environment(environment, OFFICIAL_OBSERVED_AT_MS_ENV)?,
        output: env_string(environment, OUTPUT_ENV)?
            .map(|value| parse_output(&value))
            .transpose()?,
        tracing: env_string(environment, TRACING_ENV)?
            .map(|value| parse_tracing(&value))
            .transpose()?,
    })
}

fn read_tui_environment(environment: &impl Environment) -> Result<TuiOverrides, CliError> {
    Ok(TuiOverrides {
        network: read_network_environment(environment)?,
        coin: env_string(environment, COIN_ENV)?,
        capture: env_string(environment, CAPTURE_ENV)?.map(PathBuf::from),
        dataset: env_string(environment, DATASET_ENV)?.map(PathBuf::from),
        refresh_ms: env_string(environment, REFRESH_MS_ENV)?
            .map(|value| {
                value.parse::<u64>().map_err(|error| {
                    CliError::with_source(
                        ErrorCategory::Configuration,
                        format!("invalid {REFRESH_MS_ENV} value {value:?}; expected milliseconds"),
                        error,
                    )
                })
            })
            .transpose()?,
        color: env_string(environment, COLOR_ENV)?
            .map(|value| parse_color(&value))
            .transpose()?,
        tracing: env_string(environment, TRACING_ENV)?
            .map(|value| parse_tracing(&value))
            .transpose()?,
    })
}

fn read_i64_environment(
    environment: &impl Environment,
    name: &'static str,
) -> Result<Option<i64>, CliError> {
    env_string(environment, name)?
        .map(|value| {
            value.parse::<i64>().map_err(|error| {
                CliError::with_source(
                    ErrorCategory::Configuration,
                    format!("invalid {name} value {value:?}; expected Unix milliseconds"),
                    error,
                )
            })
        })
        .transpose()
}

fn read_network_environment(environment: &impl Environment) -> Result<Option<Network>, CliError> {
    env_string(environment, NETWORK_ENV)?
        .map(|value| {
            Network::from_str(&value).map_err(|error| {
                CliError::with_source(
                    ErrorCategory::Configuration,
                    format!("invalid {NETWORK_ENV} value {value:?}"),
                    error,
                )
            })
        })
        .transpose()
}

fn env_string(
    environment: &impl Environment,
    name: &'static str,
) -> Result<Option<String>, CliError> {
    environment
        .get(name)
        .map(|value| {
            value.into_string().map_err(|_| {
                CliError::new(
                    ErrorCategory::Configuration,
                    format!("{name} must contain valid UTF-8"),
                )
            })
        })
        .transpose()
}

fn parse_output(value: &str) -> Result<OutputFormat, CliError> {
    match value {
        "human" => Ok(OutputFormat::Human),
        "json" => Ok(OutputFormat::Json),
        _ => Err(CliError::new(
            ErrorCategory::Configuration,
            format!("invalid {OUTPUT_ENV} value {value:?}; expected `human` or `json`"),
        )),
    }
}

fn parse_tracing(value: &str) -> Result<TracingMode, CliError> {
    match value {
        "quiet" => Ok(TracingMode::Quiet),
        "normal" => Ok(TracingMode::Normal),
        "verbose" => Ok(TracingMode::Verbose),
        "diagnostic" => Ok(TracingMode::Diagnostic),
        _ => Err(CliError::new(
            ErrorCategory::Configuration,
            format!(
                "invalid {TRACING_ENV} value {value:?}; expected quiet, normal, verbose, or diagnostic"
            ),
        )),
    }
}

fn parse_color(value: &str) -> Result<ColorMode, CliError> {
    match value {
        "auto" => Ok(ColorMode::Auto),
        "always" => Ok(ColorMode::Always),
        "never" => Ok(ColorMode::Never),
        _ => Err(CliError::new(
            ErrorCategory::Configuration,
            format!("invalid {COLOR_ENV} value {value:?}; expected auto, always, or never"),
        )),
    }
}

fn resolve_layers(
    cli: SnapshotOverrides,
    environment: SnapshotOverrides,
    file: SnapshotFileConfig,
) -> Result<SnapshotOptions, CliError> {
    let network = cli
        .network
        .or(environment.network)
        .or(file.network)
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::Configuration,
                "network is required; use --network, HYPERCARRY_NETWORK, or snapshot.network in the JSON config file",
            )
        })?;
    let coin = cli
        .coin
        .or(environment.coin)
        .or(file.coin)
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::Configuration,
                "coin is required; use --coin, HYPERCARRY_COIN, or snapshot.coin in the JSON config file",
            )
        })?;
    if coin.is_empty() {
        return Err(CliError::new(
            ErrorCategory::Configuration,
            "coin must not be empty",
        ));
    }

    Ok(SnapshotOptions {
        network,
        coin,
        output: cli
            .output
            .or(environment.output)
            .or(file.output)
            .unwrap_or(OutputFormat::Human),
        tracing: cli
            .tracing
            .or(environment.tracing)
            .or(file.tracing)
            .unwrap_or(TracingMode::Normal),
    })
}

fn resolve_backfill_layers(
    cli: BackfillOverrides,
    environment: BackfillOverrides,
    file: BackfillFileConfig,
) -> Result<BackfillOptions, CliError> {
    let network = required_network(cli.network, environment.network, file.network, "backfill")?;
    let coin = required_coin(cli.coin, environment.coin, file.coin, "backfill")?;
    let days = cli
        .days
        .or(environment.days)
        .or(file.days)
        .unwrap_or(DEFAULT_BACKFILL_DAYS);
    if days == 0 {
        return Err(CliError::new(
            ErrorCategory::Configuration,
            "backfill days must be greater than zero",
        ));
    }
    Ok(BackfillOptions {
        network,
        coin,
        days,
        dataset: cli
            .dataset
            .or(environment.dataset)
            .or(file.dataset)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATASET_PATH)),
        output: cli
            .output
            .or(environment.output)
            .or(file.output)
            .unwrap_or(OutputFormat::Human),
        tracing: cli
            .tracing
            .or(environment.tracing)
            .or(file.tracing)
            .unwrap_or(TracingMode::Normal),
    })
}

fn resolve_apr_layers(
    cli: AprOverrides,
    environment: AprOverrides,
    file: AprFileConfig,
) -> Result<AprOptions, CliError> {
    Ok(AprOptions {
        network: required_network(cli.network, environment.network, file.network, "apr")?,
        coin: required_coin(cli.coin, environment.coin, file.coin, "apr")?,
        dataset: cli
            .dataset
            .or(environment.dataset)
            .or(file.dataset)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATASET_PATH)),
        output: cli
            .output
            .or(environment.output)
            .or(file.output)
            .unwrap_or(OutputFormat::Human),
        tracing: cli
            .tracing
            .or(environment.tracing)
            .or(file.tracing)
            .unwrap_or(TracingMode::Normal),
    })
}

fn resolve_record_layers(
    cli: RecordOverrides,
    environment: RecordOverrides,
    file: RecordFileConfig,
) -> Result<RecordOptions, CliError> {
    let network = required_network(cli.network, environment.network, file.network, "record")?;
    let coins = cli.coins.or(environment.coins).or(file.coins).ok_or_else(|| {
        CliError::new(
            ErrorCategory::Configuration,
            format!(
                "coins are required; use --coins, {COINS_ENV}, or record.coins in the JSON config file"
            ),
        )
    })?;
    if coins.is_empty() || coins.iter().any(|coin| coin.trim().is_empty()) {
        return Err(CliError::new(
            ErrorCategory::Configuration,
            "record coins must contain at least one non-empty symbol",
        ));
    }
    let queue_capacity = cli
        .queue_capacity
        .or(environment.queue_capacity)
        .or(file.queue_capacity)
        .unwrap_or(1_024);
    if queue_capacity == 0 {
        return Err(CliError::new(
            ErrorCategory::Configuration,
            "record queue capacity must be greater than zero",
        ));
    }
    Ok(RecordOptions {
        network,
        coins,
        dataset: cli
            .dataset
            .or(environment.dataset)
            .or(file.dataset)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATASET_PATH)),
        queue_capacity,
        output: cli
            .output
            .or(environment.output)
            .or(file.output)
            .unwrap_or(OutputFormat::Human),
        tracing: cli
            .tracing
            .or(environment.tracing)
            .or(file.tracing)
            .unwrap_or(TracingMode::Normal),
    })
}

fn resolve_predict_layers(
    cli: PredictOverrides,
    environment: PredictOverrides,
    file: PredictFileConfig,
) -> Result<PredictOptions, CliError> {
    let network = required_network(cli.network, environment.network, file.network, "predict")?;
    let coin = required_coin(cli.coin, environment.coin, file.coin, "predict")?;
    let capture = cli
        .capture
        .or(environment.capture)
        .or(file.capture)
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::Configuration,
                format!(
                    "raw capture is required; use --capture, {CAPTURE_ENV}, or predict.capture in the JSON config file"
                ),
            )
        })?;
    let settlement_ms = required_timestamp(
        cli.settlement_ms,
        environment.settlement_ms,
        file.settlement_ms,
        "settlement-ms",
        SETTLEMENT_MS_ENV,
        "settlement_ms",
    )?;
    let as_of_ms = required_timestamp(
        cli.as_of_ms,
        environment.as_of_ms,
        file.as_of_ms,
        "as-of-ms",
        AS_OF_MS_ENV,
        "as_of_ms",
    )?;
    let official_rate = cli
        .official_rate
        .or(environment.official_rate)
        .or(file.official_rate);
    let official_observed_at_ms = cli
        .official_observed_at_ms
        .or(environment.official_observed_at_ms)
        .or(file.official_observed_at_ms);
    if official_rate.is_some() != official_observed_at_ms.is_some() {
        return Err(CliError::new(
            ErrorCategory::Configuration,
            "official-rate and official-observed-at-ms must be supplied together",
        ));
    }
    Ok(PredictOptions {
        network,
        coin,
        capture,
        settlement_ms,
        as_of_ms,
        dataset: cli
            .dataset
            .or(environment.dataset)
            .or(file.dataset)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATASET_PATH)),
        official_rate,
        official_observed_at_ms,
        output: cli
            .output
            .or(environment.output)
            .or(file.output)
            .unwrap_or(OutputFormat::Human),
        tracing: cli
            .tracing
            .or(environment.tracing)
            .or(file.tracing)
            .unwrap_or(TracingMode::Normal),
    })
}

fn resolve_tui_layers(
    cli: TuiOverrides,
    environment: TuiOverrides,
    file: TuiFileConfig,
) -> Result<TuiOptions, CliError> {
    let refresh_ms = cli
        .refresh_ms
        .or(environment.refresh_ms)
        .or(file.refresh_ms)
        .unwrap_or(DEFAULT_TUI_REFRESH_MS);
    if !(100..=60_000).contains(&refresh_ms) {
        return Err(CliError::new(
            ErrorCategory::Configuration,
            "TUI refresh-ms must be between 100 and 60000",
        ));
    }
    Ok(TuiOptions {
        network: required_network(cli.network, environment.network, file.network, "tui")?,
        coin: required_coin(cli.coin, environment.coin, file.coin, "tui")?,
        capture: cli
            .capture
            .or(environment.capture)
            .or(file.capture)
            .ok_or_else(|| {
                CliError::new(
                    ErrorCategory::Configuration,
                    format!(
                        "raw capture is required; use --capture, {CAPTURE_ENV}, or tui.capture in the JSON config file"
                    ),
                )
            })?,
        dataset: cli
            .dataset
            .or(environment.dataset)
            .or(file.dataset)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DATASET_PATH)),
        refresh_ms,
        color: cli
            .color
            .or(environment.color)
            .or(file.color)
            .unwrap_or(ColorMode::Auto),
        tracing: cli
            .tracing
            .or(environment.tracing)
            .or(file.tracing)
            .unwrap_or(TracingMode::Quiet),
    })
}

fn required_timestamp(
    cli: Option<i64>,
    environment: Option<i64>,
    file: Option<i64>,
    flag: &'static str,
    environment_name: &'static str,
    file_field: &'static str,
) -> Result<i64, CliError> {
    cli.or(environment).or(file).ok_or_else(|| {
        CliError::new(
            ErrorCategory::Configuration,
            format!(
                "prediction timestamp is required; use --{flag}, {environment_name}, or predict.{file_field} in the JSON config file"
            ),
        )
    })
}

fn required_network(
    cli: Option<Network>,
    environment: Option<Network>,
    file: Option<Network>,
    section: &'static str,
) -> Result<Network, CliError> {
    cli.or(environment).or(file).ok_or_else(|| {
        CliError::new(
            ErrorCategory::Configuration,
            format!(
                "network is required; use --network, {NETWORK_ENV}, or {section}.network in the JSON config file"
            ),
        )
    })
}

fn required_coin(
    cli: Option<String>,
    environment: Option<String>,
    file: Option<String>,
    section: &'static str,
) -> Result<String, CliError> {
    let coin = cli.or(environment).or(file).ok_or_else(|| {
        CliError::new(
            ErrorCategory::Configuration,
            format!(
                "coin is required; use --coin, {COIN_ENV}, or {section}.coin in the JSON config file"
            ),
        )
    })?;
    if coin.is_empty() {
        return Err(CliError::new(
            ErrorCategory::Configuration,
            "coin must not be empty",
        ));
    }
    Ok(coin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MapEnvironment(HashMap<String, OsString>);

    impl Environment for MapEnvironment {
        fn get(&self, name: &str) -> Option<OsString> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn precedence_is_cli_then_environment_then_config_file() {
        let file = parse_file_config(
            r#"{
                "snapshot": {
                    "network": "mainnet",
                    "coin": "FILE",
                    "output": "human",
                    "tracing": "quiet"
                }
            }"#,
        )
        .expect("file config parses");
        let environment = SnapshotOverrides {
            network: Some(Network::Testnet),
            coin: Some("ENV".to_owned()),
            output: Some(OutputFormat::Json),
            tracing: Some(TracingMode::Verbose),
        };
        let cli = SnapshotOverrides {
            network: Some(Network::Mainnet),
            coin: Some("CLI".to_owned()),
            output: None,
            tracing: Some(TracingMode::Diagnostic),
        };

        let resolved = resolve_layers(cli, environment, file.snapshot).expect("config resolves");

        assert_eq!(resolved.network, Network::Mainnet);
        assert_eq!(resolved.coin, "CLI");
        assert_eq!(resolved.output, OutputFormat::Json);
        assert_eq!(resolved.tracing, TracingMode::Diagnostic);
    }

    #[test]
    fn absent_network_is_a_configuration_error_with_stable_exit_code() {
        let error = resolve_layers(
            SnapshotOverrides {
                coin: Some("BTC".to_owned()),
                ..SnapshotOverrides::default()
            },
            SnapshotOverrides::default(),
            SnapshotFileConfig::default(),
        )
        .expect_err("network must never default silently");

        assert_eq!(error.category(), ErrorCategory::Configuration);
        assert_eq!(error.exit_code(), 10);
    }

    #[test]
    fn malformed_or_unknown_config_fields_are_rejected() {
        assert!(parse_file_config("not json").is_err());
        assert!(
            parse_file_config(r#"{"snapshot":{"network":"testnet","surprise":true}}"#).is_err()
        );
    }

    #[test]
    fn invalid_environment_value_is_a_configuration_error() {
        let environment = MapEnvironment(HashMap::from([(
            NETWORK_ENV.to_owned(),
            OsString::from("production"),
        )]));

        let error = read_environment(&environment).expect_err("invalid network must fail");

        assert_eq!(error.category(), ErrorCategory::Configuration);
        assert!(error.to_string().contains(NETWORK_ENV));
    }

    #[test]
    fn backfill_precedence_covers_dataset_days_and_output() {
        let file = parse_file_config(
            r#"{
                "backfill": {
                    "network": "mainnet",
                    "coin": "FILE",
                    "days": 90,
                    "dataset": "file-data",
                    "output": "human",
                    "tracing": "quiet"
                }
            }"#,
        )
        .expect("file config parses");
        let environment = BackfillOverrides {
            network: Some(Network::Testnet),
            coin: Some("ENV".to_owned()),
            days: Some(60),
            dataset: Some(PathBuf::from("env-data")),
            output: Some(OutputFormat::Json),
            tracing: Some(TracingMode::Verbose),
        };
        let cli = BackfillOverrides {
            coin: Some("CLI".to_owned()),
            days: Some(7),
            tracing: Some(TracingMode::Diagnostic),
            ..BackfillOverrides::default()
        };

        let resolved = resolve_backfill_layers(cli, environment, file.backfill)
            .expect("backfill config resolves");

        assert_eq!(resolved.network, Network::Testnet);
        assert_eq!(resolved.coin, "CLI");
        assert_eq!(resolved.days, 7);
        assert_eq!(resolved.dataset, PathBuf::from("env-data"));
        assert_eq!(resolved.output, OutputFormat::Json);
        assert_eq!(resolved.tracing, TracingMode::Diagnostic);
    }

    #[test]
    fn backfill_rejects_a_zero_day_window() {
        let error = resolve_backfill_layers(
            BackfillOverrides {
                network: Some(Network::Testnet),
                coin: Some("BTC".to_owned()),
                days: Some(0),
                ..BackfillOverrides::default()
            },
            BackfillOverrides::default(),
            BackfillFileConfig::default(),
        )
        .expect_err("zero days must fail");

        assert_eq!(error.category(), ErrorCategory::Configuration);
        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn apr_uses_file_values_and_safe_display_defaults() {
        let file = parse_file_config(
            r#"{
                "apr": {
                    "network": "testnet",
                    "coin": "BTC",
                    "dataset": "research-data"
                }
            }"#,
        )
        .expect("file config parses");

        let resolved =
            resolve_apr_layers(AprOverrides::default(), AprOverrides::default(), file.apr)
                .expect("APR config resolves");

        assert_eq!(resolved.network, Network::Testnet);
        assert_eq!(resolved.coin, "BTC");
        assert_eq!(resolved.dataset, PathBuf::from("research-data"));
        assert_eq!(resolved.output, OutputFormat::Human);
        assert_eq!(resolved.tracing, TracingMode::Normal);
    }

    #[test]
    fn record_precedence_covers_bounded_queue_and_coin_set() {
        let file = parse_file_config(
            r#"{
                "record": {
                    "network": "mainnet",
                    "coins": ["FILE"],
                    "dataset": "file-data",
                    "queue_capacity": 64,
                    "output": "human",
                    "tracing": "quiet"
                }
            }"#,
        )
        .expect("file config parses");
        let environment = RecordOverrides {
            network: Some(Network::Testnet),
            coins: Some(vec!["ENV".to_owned()]),
            dataset: Some(PathBuf::from("env-data")),
            queue_capacity: Some(128),
            output: Some(OutputFormat::Json),
            tracing: Some(TracingMode::Verbose),
        };
        let cli = RecordOverrides {
            coins: Some(vec!["BTC".to_owned(), "ETH".to_owned()]),
            queue_capacity: Some(32),
            tracing: Some(TracingMode::Diagnostic),
            ..RecordOverrides::default()
        };

        let resolved =
            resolve_record_layers(cli, environment, file.record).expect("record config resolves");

        assert_eq!(resolved.network, Network::Testnet);
        assert_eq!(resolved.coins, ["BTC", "ETH"]);
        assert_eq!(resolved.dataset, PathBuf::from("env-data"));
        assert_eq!(resolved.queue_capacity, 32);
        assert_eq!(resolved.output, OutputFormat::Json);
        assert_eq!(resolved.tracing, TracingMode::Diagnostic);
    }

    #[test]
    fn record_rejects_zero_queue_capacity() {
        let error = resolve_record_layers(
            RecordOverrides {
                network: Some(Network::Testnet),
                coins: Some(vec!["BTC".to_owned()]),
                queue_capacity: Some(0),
                ..RecordOverrides::default()
            },
            RecordOverrides::default(),
            RecordFileConfig::default(),
        )
        .expect_err("zero capacity would make the queue unusable");

        assert_eq!(error.category(), ErrorCategory::Configuration);
        assert!(error.to_string().contains("greater than zero"));
    }

    #[test]
    fn predict_precedence_covers_replay_cutoff_and_benchmark() {
        let file = parse_file_config(
            r#"{
                "predict": {
                    "network": "mainnet",
                    "coin": "FILE",
                    "capture": "file.jsonl",
                    "settlement_ms": 3600000,
                    "as_of_ms": 1000,
                    "dataset": "file-data",
                    "official_rate": "0.001",
                    "official_observed_at_ms": 900
                }
            }"#,
        )
        .expect("file config parses");
        let environment = PredictOverrides {
            network: Some(Network::Testnet),
            coin: Some("ENV".to_owned()),
            capture: Some(PathBuf::from("env.jsonl")),
            dataset: Some(PathBuf::from("env-data")),
            output: Some(OutputFormat::Json),
            ..PredictOverrides::default()
        };
        let cli = PredictOverrides {
            coin: Some("BTC".to_owned()),
            capture: Some(PathBuf::from("cli.jsonl")),
            as_of_ms: Some(2_000),
            ..PredictOverrides::default()
        };

        let resolved = resolve_predict_layers(cli, environment, file.predict)
            .expect("predict config resolves");
        assert_eq!(resolved.network, Network::Testnet);
        assert_eq!(resolved.coin, "BTC");
        assert_eq!(resolved.capture, PathBuf::from("cli.jsonl"));
        assert_eq!(resolved.settlement_ms, 3_600_000);
        assert_eq!(resolved.as_of_ms, 2_000);
        assert_eq!(resolved.dataset, PathBuf::from("env-data"));
        assert_eq!(resolved.official_rate, Some("0.001".parse().unwrap()));
        assert_eq!(resolved.official_observed_at_ms, Some(900));
        assert_eq!(resolved.output, OutputFormat::Json);
    }

    #[test]
    fn predict_rejects_an_unpaired_official_benchmark() {
        let error = resolve_predict_layers(
            PredictOverrides {
                network: Some(Network::Testnet),
                coin: Some("BTC".to_owned()),
                capture: Some(PathBuf::from("capture.jsonl")),
                settlement_ms: Some(3_600_000),
                as_of_ms: Some(1_000),
                official_rate: Some("0.001".parse().unwrap()),
                ..PredictOverrides::default()
            },
            PredictOverrides::default(),
            PredictFileConfig::default(),
        )
        .expect_err("benchmark rate without observation time could leak future data");
        assert_eq!(error.category(), ErrorCategory::Configuration);
        assert!(error.to_string().contains("must be supplied together"));
    }

    #[test]
    fn tui_precedence_covers_refresh_color_and_capture() {
        let file = parse_file_config(
            r#"{
                "tui": {
                    "network": "mainnet",
                    "coin": "FILE",
                    "capture": "file.jsonl",
                    "dataset": "file-data",
                    "refresh_ms": 5000,
                    "color": "never",
                    "tracing": "quiet"
                }
            }"#,
        )
        .expect("file config parses");
        let environment = TuiOverrides {
            network: Some(Network::Testnet),
            coin: Some("ENV".to_owned()),
            capture: Some(PathBuf::from("env.jsonl")),
            dataset: Some(PathBuf::from("env-data")),
            refresh_ms: Some(2_000),
            color: Some(ColorMode::Auto),
            tracing: Some(TracingMode::Verbose),
        };
        let cli = TuiOverrides {
            coin: Some("BTC".to_owned()),
            capture: Some(PathBuf::from("cli.jsonl")),
            refresh_ms: Some(250),
            color: Some(ColorMode::Always),
            tracing: Some(TracingMode::Diagnostic),
            ..TuiOverrides::default()
        };

        let resolved = resolve_tui_layers(cli, environment, file.tui).expect("TUI config resolves");
        assert_eq!(resolved.network, Network::Testnet);
        assert_eq!(resolved.coin, "BTC");
        assert_eq!(resolved.capture, PathBuf::from("cli.jsonl"));
        assert_eq!(resolved.dataset, PathBuf::from("env-data"));
        assert_eq!(resolved.refresh_ms, 250);
        assert_eq!(resolved.color, ColorMode::Always);
        assert_eq!(resolved.tracing, TracingMode::Diagnostic);
    }

    #[test]
    fn tui_rejects_refresh_intervals_that_could_spin_or_stall() {
        for refresh_ms in [99, 60_001] {
            let error = resolve_tui_layers(
                TuiOverrides {
                    network: Some(Network::Testnet),
                    coin: Some("BTC".to_owned()),
                    capture: Some(PathBuf::from("capture.jsonl")),
                    refresh_ms: Some(refresh_ms),
                    ..TuiOverrides::default()
                },
                TuiOverrides::default(),
                TuiFileConfig::default(),
            )
            .expect_err("unsafe refresh interval must fail");
            assert_eq!(error.category(), ErrorCategory::Configuration);
            assert!(error.to_string().contains("between 100 and 60000"));
        }
    }
}
