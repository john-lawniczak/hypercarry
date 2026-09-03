use crate::{
    config::OutputFormat,
    error::{CliError, ErrorCategory},
};
use hypercarry_core::metrics::{FundingInterval, METRICS_SCHEMA_VERSION, hourly_spread};
use rust_decimal::Decimal;
use serde::Serialize;
use std::io::{self, Write};

#[derive(Debug, PartialEq, Eq, Serialize)]
struct SpreadOutput<'a> {
    schema_version: u32,
    coin: &'a str,
    venue_a: &'a str,
    venue_b: &'a str,
    rate_a: String,
    interval_a_hours: u32,
    rate_b: String,
    interval_b_hours: u32,
    hourly_spread_a_minus_b: String,
    hourly_spread_bps: String,
}

#[allow(clippy::too_many_arguments, clippy::similar_names)]
pub fn run(
    coin: &str,
    venue_a: &str,
    rate_a: Decimal,
    interval_a_hours: u32,
    venue_b: &str,
    rate_b: Decimal,
    interval_b_hours: u32,
    format: OutputFormat,
) -> Result<(), CliError> {
    let stdout = io::stdout();
    write_spread(
        &mut stdout.lock(),
        coin,
        venue_a,
        rate_a,
        interval_a_hours,
        venue_b,
        rate_b,
        interval_b_hours,
        format,
    )
}

#[allow(clippy::too_many_arguments, clippy::similar_names)]
fn write_spread(
    output: &mut impl Write,
    coin: &str,
    venue_a: &str,
    rate_a: Decimal,
    interval_a_hours: u32,
    venue_b: &str,
    rate_b: Decimal,
    interval_b_hours: u32,
    format: OutputFormat,
) -> Result<(), CliError> {
    let interval_a = interval(interval_a_hours, "venue-a")?;
    let interval_b = interval(interval_b_hours, "venue-b")?;
    let spread = hourly_spread(rate_a, interval_a, rate_b, interval_b);
    let document = SpreadOutput {
        schema_version: METRICS_SCHEMA_VERSION,
        coin,
        venue_a,
        venue_b,
        rate_a: rate_a.normalize().to_string(),
        interval_a_hours,
        rate_b: rate_b.normalize().to_string(),
        interval_b_hours,
        hourly_spread_a_minus_b: spread.normalize().to_string(),
        hourly_spread_bps: (spread * Decimal::from(10_000_u32)).normalize().to_string(),
    };
    match format {
        OutputFormat::Human => writeln!(output, "{coin} FUNDING SPREAD")
            .and_then(|()| {
                writeln!(
                    output,
                    "{} normalized   {} per hour",
                    venue_a,
                    interval_a.to_hourly(rate_a).normalize()
                )?;
                writeln!(
                    output,
                    "{} normalized   {} per hour",
                    venue_b,
                    interval_b.to_hourly(rate_b).normalize()
                )?;
                writeln!(
                    output,
                    "{} - {}          {} per hour ({} bps)",
                    venue_a, venue_b, document.hourly_spread_a_minus_b, document.hourly_spread_bps
                )
            })
            .map_err(output_error),
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *output, &document).map_err(|error| {
                CliError::with_source(
                    ErrorCategory::Output,
                    "could not serialize spread JSON",
                    error,
                )
            })?;
            writeln!(output).map_err(output_error)
        }
    }
}

fn interval(hours: u32, flag: &'static str) -> Result<FundingInterval, CliError> {
    FundingInterval::from_hours(hours).ok_or_else(|| {
        CliError::new(
            ErrorCategory::Configuration,
            format!("--{flag}-interval-hours must be greater than zero"),
        )
    })
}

fn output_error(error: io::Error) -> CliError {
    CliError::with_source(
        ErrorCategory::Output,
        "could not write spread output",
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_normalizes_intervals_and_renders_stable_json() {
        let mut output = Vec::new();
        write_spread(
            &mut output,
            "BTC",
            "Hyperliquid",
            "0.0001".parse().unwrap(),
            1,
            "Venue8h",
            "0.0004".parse().unwrap(),
            8,
            OutputFormat::Json,
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["hourly_spread_a_minus_b"], "0.00005");
        assert_eq!(json["hourly_spread_bps"], "0.5");
    }

    #[test]
    fn spread_rejects_zero_hour_interval() {
        let error = write_spread(
            &mut Vec::new(),
            "BTC",
            "A",
            Decimal::ZERO,
            0,
            "B",
            Decimal::ZERO,
            1,
            OutputFormat::Human,
        )
        .expect_err("zero interval cannot be normalized");
        assert_eq!(error.category(), ErrorCategory::Configuration);
        assert!(error.to_string().contains("greater than zero"));
    }
}
