use crate::{
    config::OutputFormat,
    error::{CliError, ErrorCategory},
};
use hypercarry_core::metrics::{BasisError, METRICS_SCHEMA_VERSION, basis};
use rust_decimal::Decimal;
use serde::Serialize;
use std::io::{self, Write};

#[derive(Debug, PartialEq, Eq, Serialize)]
struct BasisOutput<'a> {
    schema_version: u32,
    coin: &'a str,
    quote_unit: &'static str,
    perp_mark: String,
    spot_mid: String,
    absolute_quote: String,
    ratio: String,
    ratio_percent: String,
}

pub fn run(
    coin: &str,
    perp_mark: Decimal,
    spot_mid: Decimal,
    format: OutputFormat,
) -> Result<(), CliError> {
    let stdout = io::stdout();
    write_basis(&mut stdout.lock(), coin, perp_mark, spot_mid, format)
}

fn write_basis(
    output: &mut impl Write,
    coin: &str,
    perp_mark: Decimal,
    spot_mid: Decimal,
    format: OutputFormat,
) -> Result<(), CliError> {
    let value = basis(perp_mark, spot_mid).map_err(|error| match error {
        BasisError::NonPositiveSpot { spot_mid } => CliError::new(
            ErrorCategory::Configuration,
            format!("spot mid must be positive to calculate basis, got {spot_mid} USDC"),
        ),
        BasisError::Overflow => CliError::new(
            ErrorCategory::Configuration,
            "perp mark and spot mid produce a basis outside the representable decimal range"
                .to_owned(),
        ),
    })?;
    let document = BasisOutput {
        schema_version: METRICS_SCHEMA_VERSION,
        coin,
        quote_unit: "USDC",
        perp_mark: perp_mark.normalize().to_string(),
        spot_mid: spot_mid.normalize().to_string(),
        absolute_quote: value.absolute.normalize().to_string(),
        ratio: value.ratio.normalize().to_string(),
        ratio_percent: (value.ratio * Decimal::from(100_u32))
            .normalize()
            .to_string(),
    };
    match format {
        OutputFormat::Human => writeln!(output, "{coin}-PERP BASIS")
            .and_then(|()| {
                writeln!(output, "Perp mark        {} USDC", document.perp_mark)?;
                writeln!(output, "Spot mid         {} USDC", document.spot_mid)?;
                writeln!(output, "Absolute basis   {} USDC", document.absolute_quote)?;
                writeln!(output, "Basis ratio      {}%", document.ratio_percent)
            })
            .map_err(output_error),
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *output, &document).map_err(|error| {
                CliError::with_source(
                    ErrorCategory::Output,
                    "could not serialize basis JSON",
                    error,
                )
            })?;
            writeln!(output).map_err(output_error)
        }
    }
}

fn output_error(error: io::Error) -> CliError {
    CliError::with_source(ErrorCategory::Output, "could not write basis output", error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basis_renders_deterministic_units_and_json_contract() {
        let mut human = Vec::new();
        write_basis(
            &mut human,
            "BTC",
            "101".parse().unwrap(),
            "100".parse().unwrap(),
            OutputFormat::Human,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(human).unwrap(),
            "BTC-PERP BASIS\nPerp mark        101 USDC\nSpot mid         100 USDC\nAbsolute basis   1 USDC\nBasis ratio      1%\n"
        );

        let mut json = Vec::new();
        write_basis(
            &mut json,
            "BTC",
            "101".parse().unwrap(),
            "100".parse().unwrap(),
            OutputFormat::Json,
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["quote_unit"], "USDC");
        assert_eq!(json["ratio"], "0.01");
    }

    #[test]
    fn basis_rejects_non_positive_spot_as_configuration() {
        let error = write_basis(
            &mut Vec::new(),
            "BTC",
            Decimal::ONE,
            Decimal::ZERO,
            OutputFormat::Human,
        )
        .expect_err("zero spot makes the ratio undefined");
        assert_eq!(error.category(), ErrorCategory::Configuration);
        assert!(error.to_string().contains("must be positive"));
    }
}
