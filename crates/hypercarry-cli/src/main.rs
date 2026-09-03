//! `hypercarry` command-line interface.

#![warn(missing_docs)]

mod apr;
mod artifacts;
mod backfill;
mod basis;
mod config;
mod error;
mod predict;
mod record;
mod snapshot;
mod spread;
mod tui;

use crate::{
    config::{
        AprOverrides, BackfillOverrides, ColorMode, OutputFormat, PredictOverrides,
        ProcessEnvironment, RecordOverrides, SnapshotOverrides, TracingMode, TuiOverrides,
        resolve_apr, resolve_backfill, resolve_predict, resolve_record, resolve_snapshot,
        resolve_tui,
    },
    error::{CliError, ErrorCategory},
};
use clap::{Parser, Subcommand};
use clap_complete::Shell;
use hypercarry_core::info::Network;
use rust_decimal::Decimal;
use std::{path::PathBuf, process};

const EXIT_HELP: &str = "Exit codes: 0 success; 1 internal; 2 usage; 3 unimplemented; \
10 configuration; 11 network; 12 schema; 13 storage; 14 partial-data; 15 output; 130 cancelled.";
const LONG_HELP: &str = "Examples:\n  hypercarry snapshot --network testnet --coin BTC\n  \
hypercarry backfill --network testnet --coin BTC --days 7 --dataset data\n  \
hypercarry record --network testnet --coins BTC,ETH --dataset data\n  \
hypercarry predict --network testnet --coin BTC --capture raw.jsonl \
--settlement-ms 1787619600000 --as-of-ms 1787619300000\n  \
hypercarry completions bash\n  hypercarry manpage > hypercarry.1\n\nExit codes: 0 success; 1 internal; 2 usage; 3 unimplemented; 10 configuration; 11 network; 12 schema; 13 storage; 14 partial-data; 15 output; 130 cancelled.";

/// Hyperliquid funding recorder & predictor.
#[derive(Debug, Parser)]
#[command(name = "hypercarry", version, about, after_help = EXIT_HELP, after_long_help = LONG_HELP)]
pub(crate) struct Cli {
    /// JSON configuration file. Overrides `HYPERCARRY_CONFIG`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Fetch a live market and funding snapshot.
    Snapshot {
        /// Hyperliquid deployment to query; no implicit default.
        #[arg(long)]
        network: Option<Network>,
        /// Exact perpetual coin symbol, such as BTC.
        #[arg(long)]
        coin: Option<String>,
        /// Output format.
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
        /// Metadata-only tracing mode written to stderr.
        #[arg(long, value_enum)]
        tracing: Option<TracingMode>,
    },
    /// Backfill settled funding history into the local Parquet dataset.
    Backfill {
        /// Hyperliquid deployment to query; no implicit default.
        #[arg(long)]
        network: Option<Network>,
        /// Exact perpetual coin symbol, such as BTC.
        #[arg(long)]
        coin: Option<String>,
        /// Inclusive lookback window in days. Defaults to 30.
        #[arg(long)]
        days: Option<u32>,
        /// Local dataset root. Defaults to ./data.
        #[arg(long)]
        dataset: Option<PathBuf>,
        /// Completion summary format.
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
        /// Metadata-only tracing mode written to stderr.
        #[arg(long, value_enum)]
        tracing: Option<TracingMode>,
    },
    /// Record live public asset context and L2 book frames.
    Record {
        /// Hyperliquid deployment to subscribe to; no implicit default.
        #[arg(long)]
        network: Option<Network>,
        /// Comma-delimited perpetual symbols, such as BTC,ETH.
        #[arg(long, value_delimiter = ',')]
        coins: Vec<String>,
        /// Local dataset root. Defaults to ./data.
        #[arg(long)]
        dataset: Option<PathBuf>,
        /// Bounded normalization queue capacity. Defaults to 1024.
        #[arg(long)]
        queue_capacity: Option<usize>,
        /// Final health summary format.
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
        /// Metadata-only tracing mode written to stderr.
        #[arg(long, value_enum)]
        tracing: Option<TracingMode>,
    },
    /// Show the latest annualized funding rate from the local dataset.
    Apr {
        /// Dataset network identity; no implicit default.
        #[arg(long)]
        network: Option<Network>,
        /// Exact perpetual coin symbol, such as BTC.
        #[arg(long)]
        coin: Option<String>,
        /// Local dataset root. Defaults to ./data.
        #[arg(long)]
        dataset: Option<PathBuf>,
        /// Output format.
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
        /// Metadata-only tracing mode written to stderr.
        #[arg(long, value_enum)]
        tracing: Option<TracingMode>,
    },
    /// Calculate exact perp-spot basis from explicit prices.
    Basis {
        /// Display symbol, such as BTC.
        #[arg(long)]
        coin: String,
        /// Perpetual mark price in USDC.
        #[arg(long)]
        perp_mark: Decimal,
        /// Spot mid price in USDC; must be positive.
        #[arg(long)]
        spot_mid: Decimal,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        output: OutputFormat,
    },
    /// Calculate an interval-normalized cross-venue funding spread.
    Spread {
        #[arg(long)]
        coin: String,
        #[arg(long)]
        venue_a: String,
        #[arg(long, allow_hyphen_values = true)]
        rate_a: Decimal,
        #[arg(long)]
        interval_a_hours: u32,
        #[arg(long)]
        venue_b: String,
        #[arg(long, allow_hyphen_values = true)]
        rate_b: Decimal,
        #[arg(long)]
        interval_b_hours: u32,
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        output: OutputFormat,
    },
    /// Replay a raw capture to predict an hourly funding settlement.
    Predict {
        /// Network identity expected in the raw capture and settled dataset.
        #[arg(long)]
        network: Option<Network>,
        /// Exact perpetual coin symbol, such as BTC.
        #[arg(long)]
        coin: Option<String>,
        /// M3 raw JSONL capture to replay.
        #[arg(long)]
        capture: Option<PathBuf>,
        /// Target UTC-hour boundary in Unix milliseconds.
        #[arg(long)]
        settlement_ms: Option<i64>,
        /// Prediction cutoff in Unix milliseconds; must precede settlement.
        #[arg(long)]
        as_of_ms: Option<i64>,
        /// Settled-funding dataset root used for realized error when available.
        #[arg(long)]
        dataset: Option<PathBuf>,
        /// Hourly official predictedFundings rate, used only as a benchmark.
        #[arg(long, allow_hyphen_values = true)]
        official_rate: Option<Decimal>,
        /// Receive time of --official-rate, required with that benchmark.
        #[arg(long)]
        official_observed_at_ms: Option<i64>,
        /// Output format.
        #[arg(long, value_enum)]
        output: Option<OutputFormat>,
        /// Metadata-only tracing mode written to stderr.
        #[arg(long, value_enum)]
        tracing: Option<TracingMode>,
    },
    /// Generate shell completion source on stdout.
    Completions {
        /// Shell whose completion script should be generated.
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Generate a roff man page on stdout.
    Manpage,
    /// Monitor the current funding hour from an actively written M3 capture.
    Tui {
        #[arg(long)]
        network: Option<Network>,
        #[arg(long)]
        coin: Option<String>,
        /// M3 raw JSONL session currently written by `hypercarry record`.
        #[arg(long)]
        capture: Option<PathBuf>,
        #[arg(long)]
        dataset: Option<PathBuf>,
        /// Refresh interval in milliseconds (100..=60000).
        #[arg(long)]
        refresh_ms: Option<u64>,
        /// Color policy; semantic labels remain visible without color.
        #[arg(long, value_enum)]
        color: Option<ColorMode>,
        #[arg(long, value_enum)]
        tracing: Option<TracingMode>,
    },
}

fn main() -> anyhow::Result<()> {
    match run() {
        Ok(()) => Ok(()),
        Err(error) if error.category() == ErrorCategory::Internal => {
            Err(anyhow::Error::new(error).context("error[internal]"))
        }
        Err(error) => {
            eprintln!("error[{}]: {error}", error.category().as_str());
            process::exit(i32::from(error.exit_code()));
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "central dispatch keeps every command's configuration boundary visible"
)]
fn run() -> Result<(), CliError> {
    let cli = Cli::parse();
    match cli.command {
        Command::Snapshot {
            network,
            coin,
            output,
            tracing,
        } => {
            let options = resolve_snapshot(
                cli.config,
                SnapshotOverrides {
                    network,
                    coin,
                    output,
                    tracing,
                },
                &ProcessEnvironment,
            )?;
            snapshot::configure_tracing(options.tracing)?;
            snapshot::run(&options)
        }
        Command::Backfill {
            network,
            coin,
            days,
            dataset,
            output,
            tracing,
        } => {
            let options = resolve_backfill(
                cli.config,
                BackfillOverrides {
                    network,
                    coin,
                    days,
                    dataset,
                    output,
                    tracing,
                },
                &ProcessEnvironment,
            )?;
            snapshot::configure_tracing(options.tracing)?;
            backfill::run(&options)
        }
        Command::Record {
            network,
            coins,
            dataset,
            queue_capacity,
            output,
            tracing,
        } => {
            let options = resolve_record(
                cli.config,
                RecordOverrides {
                    network,
                    coins: (!coins.is_empty()).then_some(coins),
                    dataset,
                    queue_capacity,
                    output,
                    tracing,
                },
                &ProcessEnvironment,
            )?;
            snapshot::configure_tracing(options.tracing)?;
            record::run(&options)
        }
        Command::Apr {
            network,
            coin,
            dataset,
            output,
            tracing,
        } => {
            let options = resolve_apr(
                cli.config,
                AprOverrides {
                    network,
                    coin,
                    dataset,
                    output,
                    tracing,
                },
                &ProcessEnvironment,
            )?;
            snapshot::configure_tracing(options.tracing)?;
            apr::run(&options)
        }
        Command::Basis {
            coin,
            perp_mark,
            spot_mid,
            output,
        } => basis::run(&coin, perp_mark, spot_mid, output),
        Command::Spread {
            coin,
            venue_a,
            rate_a,
            interval_a_hours,
            venue_b,
            rate_b,
            interval_b_hours,
            output,
        } => spread::run(
            &coin,
            &venue_a,
            rate_a,
            interval_a_hours,
            &venue_b,
            rate_b,
            interval_b_hours,
            output,
        ),
        Command::Predict {
            network,
            coin,
            capture,
            settlement_ms,
            as_of_ms,
            dataset,
            official_rate,
            official_observed_at_ms,
            output,
            tracing,
        } => run_predict(
            cli.config,
            PredictOverrides {
                network,
                coin,
                capture,
                settlement_ms,
                as_of_ms,
                dataset,
                official_rate,
                official_observed_at_ms,
                output,
                tracing,
            },
        ),
        Command::Completions { shell } => artifacts::completions(shell),
        Command::Manpage => artifacts::manpage(),
        Command::Tui {
            network,
            coin,
            capture,
            dataset,
            refresh_ms,
            color,
            tracing,
        } => {
            let options = resolve_tui(
                cli.config,
                TuiOverrides {
                    network,
                    coin,
                    capture,
                    dataset,
                    refresh_ms,
                    color,
                    tracing,
                },
                &ProcessEnvironment,
            )?;
            snapshot::configure_tracing(options.tracing)?;
            tui::run(&options)
        }
    }
}

fn run_predict(config: Option<PathBuf>, overrides: PredictOverrides) -> Result<(), CliError> {
    let options = resolve_predict(config, overrides, &ProcessEnvironment)?;
    snapshot::configure_tracing(options.tracing)?;
    predict::run(&options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, error::ErrorKind};

    #[test]
    fn cli_surface_is_valid_and_documents_exit_codes() {
        Cli::command().debug_assert();
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("10 configuration"));
        assert!(help.contains("14 partial-data"));
        assert!(help.contains("130 cancelled"));
    }

    #[test]
    fn snapshot_accepts_deferred_configuration_but_rejects_invalid_cli_network() {
        Cli::try_parse_from(["hypercarry", "snapshot"])
            .expect("environment or config file may provide required values");

        let invalid = Cli::try_parse_from([
            "hypercarry",
            "snapshot",
            "--network",
            "production",
            "--coin",
            "BTC",
        ])
        .expect_err("unknown CLI network must be rejected");
        assert_eq!(invalid.kind(), ErrorKind::ValueValidation);
    }
}
