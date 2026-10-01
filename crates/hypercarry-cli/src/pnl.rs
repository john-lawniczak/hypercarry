use crate::{
    config::{OutputFormat, PnlOptions},
    error::{CliError, ErrorCategory},
};
use chrono::{DateTime, SecondsFormat, Utc};
use hypercarry_core::carry::{
    CarryError, CarryExit, CarryPnl, CarrySide, CarryTrade, FeeSchedule, FundingSettlement,
    annualized_return,
};
use hypercarry_storage::{dataset::SettledFundingDataset, settled_funding::SettledFundingRecord};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

const VENUE: &str = "hyperliquid";
const QUOTE_UNIT: &str = "USDC";
/// Display-only rounding for derived percentages, which divide to full scale.
const PERCENT_DISPLAY_DP: u32 = 6;
const JSON_SCHEMA_VERSION: u32 = 1;
const TRADE_SCHEMA_VERSION: u32 = 1;
const MILLISECONDS_PER_HOUR: i64 = 60 * 60 * 1_000;
const MAX_SETTLEMENT_JITTER_MS: i64 = 60 * 1_000;
/// Label-column width used by the human view, so the total rule lines up.
const SEPARATOR_INDENT: &str = "                 ";

/// Side of the perpetual leg, as written in a trade document.
///
/// The CLI owns this wire format so the pure `carry` calculation stays free of
/// serialization concerns, matching how the rest of the workspace separates
/// domain types from the shapes they are transported in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TradeSide {
    Long,
    Short,
}

impl From<TradeSide> for CarrySide {
    fn from(side: TradeSide) -> Self {
        match side {
            TradeSide::Long => Self::Long,
            TradeSide::Short => Self::Short,
        }
    }
}

/// How each settlement in the window is valued.
///
/// The settled-funding dataset stores a rate and a premium, not a mark price,
/// so valuing a settlement requires a choice the operator must make explicitly.
/// The choice is echoed into the output rather than hidden.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum SettlementValuation {
    /// Value every settlement at the perpetual entry price: constant notional.
    #[default]
    PerpEntryPrice,
    /// Value every settlement at one explicit price.
    Fixed {
        #[serde(with = "rust_decimal::serde::str")]
        price: Decimal,
    },
}

impl SettlementValuation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PerpEntryPrice => "perp-entry-price",
            Self::Fixed { .. } => "fixed",
        }
    }

    fn price(self, perp_entry_price: Decimal) -> Decimal {
        match self {
            Self::PerpEntryPrice => perp_entry_price,
            Self::Fixed { price } => price,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct TradeFees {
    #[serde(with = "rust_decimal::serde::str")]
    entry_rate: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    exit_rate: Decimal,
}

impl Default for TradeFees {
    fn default() -> Self {
        Self {
            entry_rate: Decimal::ZERO,
            exit_rate: Decimal::ZERO,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct TradeExit {
    #[serde(with = "rust_decimal::serde::str")]
    perp_price: Decimal,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    spot_price: Option<Decimal>,
    time_ms: i64,
}

/// One carry position, as recorded by an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct TradeDocument {
    schema_version: u32,
    side: TradeSide,
    #[serde(with = "rust_decimal::serde::str")]
    size: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    perp_entry_price: Decimal,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    spot_entry_price: Option<Decimal>,
    entry_time_ms: i64,
    #[serde(default)]
    exit: Option<TradeExit>,
    #[serde(default)]
    fees: TradeFees,
    #[serde(default)]
    settlement_valuation: SettlementValuation,
}

impl TradeDocument {
    fn load(path: &Path) -> Result<Self, CliError> {
        let contents = fs::read_to_string(path).map_err(|error| {
            CliError::with_source(
                ErrorCategory::Configuration,
                format!("could not read trade document {}", path.display()),
                error,
            )
        })?;
        let document: Self = serde_json::from_str(&contents).map_err(|error| {
            CliError::with_source(
                ErrorCategory::Configuration,
                format!("could not parse trade document {}", path.display()),
                error,
            )
        })?;

        if document.schema_version != TRADE_SCHEMA_VERSION {
            return Err(CliError::new(
                ErrorCategory::Configuration,
                format!(
                    "trade document schema_version {} is not supported; expected {TRADE_SCHEMA_VERSION}",
                    document.schema_version
                ),
            ));
        }
        if let Some(exit) = document.exit
            && exit.time_ms <= document.entry_time_ms
        {
            return Err(CliError::new(
                ErrorCategory::Configuration,
                format!(
                    "trade exit time {}ms must be after entry time {}ms",
                    exit.time_ms, document.entry_time_ms
                ),
            ));
        }

        Ok(document)
    }

    fn to_trade(self) -> CarryTrade {
        CarryTrade {
            side: self.side.into(),
            size: self.size,
            perp_entry_price: self.perp_entry_price,
            spot_entry_price: self.spot_entry_price,
            exit: self.exit.map(|exit| CarryExit {
                perp_price: exit.perp_price,
                spot_price: exit.spot_price,
            }),
            fees: FeeSchedule {
                entry_rate: self.fees.entry_rate,
                exit_rate: self.fees.exit_rate,
            },
        }
    }

    /// Inclusive upper bound of the funding window.
    ///
    /// An open position accrues through the latest settlement the dataset holds.
    const fn window_end_ms(self) -> i64 {
        match self.exit {
            Some(exit) => exit.time_ms,
            None => i64::MAX,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct PnlView<'a> {
    schema_version: u32,
    network: hypercarry_core::info::Network,
    venue: &'static str,
    coin: &'a str,
    quote_unit: &'static str,
    side: &'static str,
    size: String,
    hedged: bool,
    closed: bool,
    settlement_valuation: &'static str,
    valuation_price: String,
    entry_time_ms: i64,
    exit_time_ms: Option<i64>,
    first_settlement_ms: Option<i64>,
    last_settlement_ms: Option<i64>,
    settlements: usize,
    dataset_first_settlement_ms: i64,
    dataset_last_settlement_ms: i64,
    window_fully_covered: bool,
    entry_notional: String,
    funding: String,
    perp_price_pnl: String,
    spot_price_pnl: String,
    fees: String,
    net: String,
    return_on_notional: String,
    annualized_return: Option<String>,
    #[serde(skip)]
    entry_time_iso: String,
    #[serde(skip)]
    exit_time_iso: Option<String>,
    #[serde(skip)]
    first_settlement_iso: Option<String>,
    #[serde(skip)]
    last_settlement_iso: Option<String>,
    #[serde(skip)]
    holding_hours: Option<Decimal>,
    #[serde(skip)]
    return_on_notional_percent: String,
    #[serde(skip)]
    annualized_return_percent: Option<String>,
}

pub fn run(options: &PnlOptions) -> Result<(), CliError> {
    let dataset = SettledFundingDataset::new(&options.dataset);
    let document = TradeDocument::load(&options.trade)?;
    let view = evaluate(&dataset, options, document)?;
    let stdout = io::stdout();
    write_pnl(&mut stdout.lock(), &view, options.output)
}

fn evaluate<'a>(
    dataset: &SettledFundingDataset,
    options: &'a PnlOptions,
    document: TradeDocument,
) -> Result<PnlView<'a>, CliError> {
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
    let (dataset_first_settlement_ms, dataset_last_settlement_ms) = records
        .first()
        .zip(records.last())
        .map(|(first, last)| {
            (
                first.identity.settlement_time.as_i64(),
                last.identity.settlement_time.as_i64(),
            )
        })
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::PartialData,
                format!(
                    "dataset contains no {} settled funding for coin {:?}; run backfill first",
                    options.network, options.coin
                ),
            )
        })?;

    let window_end_ms = document.window_end_ms();
    let valuation_price = document
        .settlement_valuation
        .price(document.perp_entry_price);
    let settlements = window_settlements(
        &records,
        document.entry_time_ms,
        window_end_ms,
        valuation_price,
    );

    let trade = document.to_trade();
    let pnl = trade.evaluate(&settlements).map_err(carry_error)?;

    let holding_hours = holding_hours(document, &pnl);
    let annualized = holding_hours
        .map(|hours| annualized_return(pnl.net, pnl.entry_notional, hours).map_err(carry_error))
        .transpose()?;
    let return_on_notional = pnl.net.checked_div(pnl.entry_notional).ok_or_else(|| {
        CliError::new(
            ErrorCategory::Schema,
            "return on notional is not representable",
        )
    })?;

    Ok(PnlView {
        schema_version: JSON_SCHEMA_VERSION,
        network: options.network,
        venue: VENUE,
        coin: &options.coin,
        quote_unit: QUOTE_UNIT,
        side: match trade.side {
            CarrySide::Long => "long",
            CarrySide::Short => "short",
        },
        size: document.size.normalize().to_string(),
        hedged: document.spot_entry_price.is_some(),
        closed: pnl.closed,
        settlement_valuation: document.settlement_valuation.as_str(),
        valuation_price: valuation_price.normalize().to_string(),
        entry_time_ms: document.entry_time_ms,
        exit_time_ms: document.exit.map(|exit| exit.time_ms),
        first_settlement_ms: pnl.first_settlement_ms,
        last_settlement_ms: pnl.last_settlement_ms,
        settlements: pnl.settlements,
        dataset_first_settlement_ms,
        dataset_last_settlement_ms,
        window_fully_covered: window_is_covered(&records, document.entry_time_ms, window_end_ms),
        entry_notional: pnl.entry_notional.normalize().to_string(),
        funding: pnl.funding.normalize().to_string(),
        perp_price_pnl: pnl.perp_price_pnl.normalize().to_string(),
        spot_price_pnl: pnl.spot_price_pnl.normalize().to_string(),
        fees: pnl.fees.normalize().to_string(),
        net: pnl.net.normalize().to_string(),
        return_on_notional: return_on_notional.normalize().to_string(),
        annualized_return: annualized.map(|value| value.normalize().to_string()),
        entry_time_iso: iso_time(document.entry_time_ms)?,
        exit_time_iso: document
            .exit
            .map(|exit| iso_time(exit.time_ms))
            .transpose()?,
        first_settlement_iso: pnl.first_settlement_ms.map(iso_time).transpose()?,
        last_settlement_iso: pnl.last_settlement_ms.map(iso_time).transpose()?,
        holding_hours,
        return_on_notional_percent: signed_percent(return_on_notional),
        annualized_return_percent: annualized.map(signed_percent),
    })
}

/// Whether the dataset can account for every settlement the window earned.
///
/// Spanning the endpoints is not enough: a settlement missing *inside* the
/// window understates funding just as much as one missing off either end, and a
/// span check cannot see it. So this requires an observation at or before entry,
/// an observation at or after the end, and an unbroken hourly sequence between
/// them. Gaps outside that bracket belong to other windows and are ignored.
///
/// Requiring an observation past the end is deliberately stricter than the
/// hourly grid demands, and it is the conservative direction: a trade that just
/// closed reads as partial until the next settlement is recorded, which also
/// distinguishes "nothing was due" from "recording stopped". Open positions
/// (`end` is [`i64::MAX`]) are never fully covered, because more funding is
/// still to come.
fn window_is_covered(records: &[SettledFundingRecord], entry: i64, end: i64) -> bool {
    let after_entry = records.partition_point(|r| r.identity.settlement_time.as_i64() <= entry);
    let Some(start) = after_entry.checked_sub(1) else {
        return false;
    };
    let finish = records.partition_point(|r| r.identity.settlement_time.as_i64() < end);
    if finish >= records.len() || start > finish {
        return false;
    }
    records[start..=finish].windows(2).all(|pair| {
        pair[1]
            .identity
            .settlement_time
            .as_i64()
            .checked_sub(pair[0].identity.settlement_time.as_i64())
            .is_some_and(|elapsed| {
                (MILLISECONDS_PER_HOUR - MAX_SETTLEMENT_JITTER_MS
                    ..=MILLISECONDS_PER_HOUR + MAX_SETTLEMENT_JITTER_MS)
                    .contains(&elapsed)
            })
    })
}

/// Settlements the position was open for.
///
/// Funding is paid by positions open at a settlement, so the window is
/// exclusive of the entry time and inclusive of the exit time: a position
/// opened exactly at a settlement did not hold through it.
fn window_settlements(
    records: &[SettledFundingRecord],
    entry_time_ms: i64,
    window_end_ms: i64,
    valuation_price: Decimal,
) -> Vec<FundingSettlement> {
    records
        .iter()
        .filter(|record| {
            let time = record.identity.settlement_time.as_i64();
            time > entry_time_ms && time <= window_end_ms
        })
        .map(|record| FundingSettlement {
            settlement_time_ms: record.identity.settlement_time.as_i64(),
            funding_rate: record.funding_rate,
            mark_price: valuation_price,
        })
        .collect()
}

/// Hours the position was held, for annualization.
///
/// A closed trade uses its recorded exit; an open one is measured to its last
/// applied settlement, because the dataset cannot attest to anything later.
fn holding_hours(document: TradeDocument, pnl: &CarryPnl) -> Option<Decimal> {
    let end_ms = match document.exit {
        Some(exit) => exit.time_ms,
        None => pnl.last_settlement_ms?,
    };
    let elapsed_ms = end_ms.checked_sub(document.entry_time_ms)?;
    if elapsed_ms <= 0 {
        return None;
    }
    Decimal::from(elapsed_ms).checked_div(Decimal::from(MILLISECONDS_PER_HOUR))
}

fn carry_error(error: CarryError) -> CliError {
    let category = match error {
        CarryError::Overflow => ErrorCategory::Schema,
        _ => ErrorCategory::Configuration,
    };
    CliError::with_source(category, "could not evaluate the carry trade", error)
}

fn iso_time(time_ms: i64) -> Result<String, CliError> {
    Ok(DateTime::<Utc>::from_timestamp_millis(time_ms)
        .ok_or_else(|| {
            CliError::new(
                ErrorCategory::Schema,
                format!("timestamp {time_ms}ms cannot be formatted as UTC"),
            )
        })?
        .to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// A percentage for the human view, rounded for display only.
///
/// Dividing a net result by notional produces a full-scale decimal, which is
/// correct but unreadable. The JSON contract keeps the exact value; only this
/// operator view rounds, and it rounds with exact decimal arithmetic rather
/// than through `f64`.
fn signed_percent(value: Decimal) -> String {
    signed((value * Decimal::from(100)).round_dp(PERCENT_DISPLAY_DP))
}

fn signed(value: Decimal) -> String {
    let value = value.normalize();
    if value.is_sign_negative() {
        value.to_string()
    } else {
        format!("+{value}")
    }
}

fn write_pnl(
    output: &mut impl Write,
    view: &PnlView<'_>,
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => {
            let result = (|| -> io::Result<()> {
                write_position(output, view)?;
                writeln!(output)?;
                write_decomposition(output, view)
            })();
            result.map_err(output_error)
        }
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *output, view).map_err(|error| {
                CliError::with_source(ErrorCategory::Output, "could not serialize P&L JSON", error)
            })?;
            writeln!(output).map_err(output_error)
        }
    }
}

/// What the position was, and which settlements the result rests on.
fn write_position(output: &mut impl Write, view: &PnlView<'_>) -> io::Result<()> {
    writeln!(
        output,
        "{}-PERP · {} {} · {} CARRY",
        view.coin,
        view.venue.to_ascii_uppercase(),
        view.network.as_str().to_ascii_uppercase(),
        view.side.to_ascii_uppercase()
    )?;
    writeln!(output)?;

    match (&view.exit_time_iso, view.holding_hours) {
        (Some(exit), Some(hours)) => writeln!(
            output,
            "{:<17}{} → {} ({}h)",
            "Window",
            view.entry_time_iso,
            exit,
            hours.normalize()
        )?,
        _ => writeln!(output, "{:<17}{} → open", "Window", view.entry_time_iso)?,
    }
    writeln!(
        output,
        "{:<17}{} applied{}",
        "Settlements",
        view.settlements,
        if view.window_fully_covered {
            ""
        } else {
            "  (dataset does not span the full window)"
        }
    )?;
    // Settlement timestamps carry millisecond jitter, so an operator comparing
    // a round-hour window against this range can see exactly what was counted.
    if let (Some(first), Some(last)) = (&view.first_settlement_iso, &view.last_settlement_iso) {
        writeln!(output, "{:<17}{} → {}", "Applied range", first, last)?;
    }
    writeln!(
        output,
        "{:<17}{} {}  (notional {} {} at entry{})",
        "Size",
        view.size,
        view.coin,
        view.entry_notional,
        view.quote_unit,
        if view.hedged {
            ", spot hedged"
        } else {
            ", unhedged"
        }
    )?;
    writeln!(
        output,
        "{:<17}{} at {}",
        "Valuation", view.settlement_valuation, view.valuation_price
    )
}

/// Where the result came from, and what it totals to.
fn write_decomposition(output: &mut impl Write, view: &PnlView<'_>) -> io::Result<()> {
    for (label, amount) in [
        ("Funding", &view.funding),
        ("Perp price", &view.perp_price_pnl),
        ("Spot hedge", &view.spot_price_pnl),
    ] {
        writeln!(
            output,
            "{:<17}{} {}",
            label,
            signed_decimal(amount),
            view.quote_unit
        )?;
    }
    writeln!(output, "{:<17}-{} {}", "Fees", view.fees, view.quote_unit)?;
    writeln!(output, "{SEPARATOR_INDENT}─────────────")?;

    let annualized = view
        .annualized_return_percent
        .as_ref()
        .map_or_else(String::new, |apr| format!(", {apr}% APR"));
    writeln!(
        output,
        "{:<17}{} {}  ({}% on notional{annualized})",
        "Net",
        signed_decimal(&view.net),
        view.quote_unit,
        view.return_on_notional_percent
    )
}

/// Re-sign an already-normalized decimal string for the human view.
fn signed_decimal(value: &str) -> String {
    value
        .parse::<Decimal>()
        .map_or_else(|_| value.to_owned(), signed)
}

fn output_error(error: io::Error) -> CliError {
    CliError::with_source(ErrorCategory::Output, "could not write P&L output", error)
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
        IngestionProvenance, RequestWindow, SourceEndpointClass,
    };
    use serde_json::Value;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("hypercarry-pnl-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).expect("temp dir is created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const HOUR_MS: i64 = 3_600_000;
    const BASE_MS: i64 = 1_789_750_800_000;

    fn record(time_ms: i64, rate: &str) -> SettledFundingRecord {
        SettledFundingRecord::from_history_entry(
            Network::Mainnet,
            VENUE,
            FundingHistoryEntry {
                coin: "BTC".to_owned(),
                funding_rate: rate.parse().expect("rate parses"),
                premium: Decimal::ZERO,
                time: TimestampMs::new(time_ms),
            },
            IngestionProvenance {
                source_endpoint_class: SourceEndpointClass::Official,
                ingestion_time: TimestampMs::new(time_ms + 1),
                request_window: RequestWindow::new(
                    TimestampMs::new(time_ms),
                    TimestampMs::new(time_ms),
                )
                .expect("window is valid"),
                software_version: "test".to_owned(),
            },
        )
        .expect("record is valid")
    }

    fn dataset_with(records: &[SettledFundingRecord], root: &Path) -> SettledFundingDataset {
        let dataset = SettledFundingDataset::new(root);
        dataset.commit(records).expect("records commit");
        dataset
    }

    fn options(temp: &TempDir, trade: PathBuf) -> PnlOptions {
        PnlOptions {
            network: Network::Mainnet,
            coin: "BTC".to_owned(),
            dataset: temp.path().to_path_buf(),
            trade,
            output: OutputFormat::Json,
            tracing: TracingMode::Normal,
        }
    }

    fn write_trade(temp: &TempDir, body: &str) -> PathBuf {
        let path = temp.path().join("trade.json");
        fs::write(&path, body).expect("trade document writes");
        path
    }

    fn hedged_short_trade() -> String {
        format!(
            r#"{{
              "schema_version": 1,
              "side": "short",
              "size": "1",
              "perp_entry_price": "80000",
              "spot_entry_price": "80000",
              "entry_time_ms": {BASE_MS}
            }}"#
        )
    }

    #[test]
    fn open_hedged_short_accrues_funding_from_the_dataset() {
        let temp = TempDir::new();
        let records = [
            record(BASE_MS, "0.0001"), // at entry: excluded
            record(BASE_MS + HOUR_MS, "0.0001"),
            record(BASE_MS + 2 * HOUR_MS, "0.0001"),
        ];
        let dataset = dataset_with(&records, temp.path());
        let trade = write_trade(&temp, &hedged_short_trade());
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");

        let view = evaluate(&dataset, &options, document).expect("evaluates");

        assert_eq!(view.settlements, 2);
        assert_eq!(view.funding, "16");
        assert_eq!(view.net, "16");
        assert!(!view.closed);
        assert!(view.hedged);
        assert_eq!(view.first_settlement_ms, Some(BASE_MS + HOUR_MS));
    }

    #[test]
    fn entry_settlement_is_excluded_and_exit_settlement_is_included() {
        let temp = TempDir::new();
        let records = [
            record(BASE_MS, "0.0001"),
            record(BASE_MS + HOUR_MS, "0.0001"),
            record(BASE_MS + 2 * HOUR_MS, "0.0001"),
        ];
        let dataset = dataset_with(&records, temp.path());
        let body = format!(
            r#"{{
              "schema_version": 1,
              "side": "short",
              "size": "1",
              "perp_entry_price": "80000",
              "spot_entry_price": "80000",
              "entry_time_ms": {BASE_MS},
              "exit": {{
                "perp_price": "80000",
                "spot_price": "80000",
                "time_ms": {}
              }}
            }}"#,
            BASE_MS + HOUR_MS
        );
        let trade = write_trade(&temp, &body);
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");

        let view = evaluate(&dataset, &options, document).expect("evaluates");

        assert_eq!(view.settlements, 1);
        assert_eq!(view.last_settlement_ms, Some(BASE_MS + HOUR_MS));
        assert!(view.closed);
    }

    #[test]
    fn partial_dataset_coverage_is_reported() {
        let temp = TempDir::new();
        let records = [record(BASE_MS + HOUR_MS, "0.0001")];
        let dataset = dataset_with(&records, temp.path());
        let body = format!(
            r#"{{
              "schema_version": 1,
              "side": "short",
              "size": "1",
              "perp_entry_price": "80000",
              "entry_time_ms": {BASE_MS},
              "exit": {{"perp_price": "80000", "time_ms": {}}}
            }}"#,
            BASE_MS + 10 * HOUR_MS
        );
        let trade = write_trade(&temp, &body);
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");

        let view = evaluate(&dataset, &options, document).expect("evaluates");

        assert!(!view.window_fully_covered);
        assert_eq!(view.dataset_last_settlement_ms, BASE_MS + HOUR_MS);
    }

    #[test]
    fn a_dataset_starting_after_entry_is_not_full_coverage() {
        // The position opened before the dataset begins, so settlements it
        // actually earned are missing and the funding total understates.
        let temp = TempDir::new();
        let records = [
            record(BASE_MS + 5 * HOUR_MS, "0.0001"),
            record(BASE_MS + 6 * HOUR_MS, "0.0001"),
        ];
        let dataset = dataset_with(&records, temp.path());
        let body = format!(
            r#"{{
              "schema_version": 1,
              "side": "short",
              "size": "1",
              "perp_entry_price": "80000",
              "entry_time_ms": {BASE_MS},
              "exit": {{"perp_price": "80000", "time_ms": {}}}
            }}"#,
            BASE_MS + 6 * HOUR_MS
        );
        let trade = write_trade(&temp, &body);
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");

        let view = evaluate(&dataset, &options, document).expect("evaluates");

        assert!(!view.window_fully_covered);
        assert_eq!(view.dataset_first_settlement_ms, BASE_MS + 5 * HOUR_MS);
        assert_eq!(view.settlements, 2);
    }

    #[test]
    fn coverage_rejects_missing_hours_inside_or_at_the_edges_of_a_trade() {
        let full: Vec<_> = (0..=5)
            .map(|hour| record(BASE_MS + hour * HOUR_MS, "0.0001"))
            .collect();
        let entry = BASE_MS + HOUR_MS / 2;
        let end = BASE_MS + 4 * HOUR_MS + HOUR_MS / 2;
        assert!(window_is_covered(&full, entry, end));
        for missing in 1..=4 {
            let mut gapped = full.clone();
            gapped.remove(missing);
            assert!(
                !window_is_covered(&gapped, entry, end),
                "missing hour {missing}"
            );
        }
        assert!(!window_is_covered(&full[1..], entry, end));
        assert!(!window_is_covered(&full[..5], entry, end));
        assert!(!window_is_covered(&full, entry, i64::MAX));
    }

    #[test]
    fn coverage_accepts_jitter_and_ignores_gaps_outside_the_trade() {
        let records = [
            record(BASE_MS - 10 * HOUR_MS, "0.0001"),
            record(BASE_MS + 7, "0.0001"),
            record(BASE_MS + HOUR_MS + 91, "0.0001"),
            record(BASE_MS + 2 * HOUR_MS + 1, "0.0001"),
            record(BASE_MS + 10 * HOUR_MS, "0.0001"),
        ];
        assert!(window_is_covered(
            &records,
            BASE_MS + 100,
            BASE_MS + 2 * HOUR_MS
        ));
        // A second record in the same hour is not evidence of another hour.
        let duplicate_hour = [record(BASE_MS, "0.0001"), record(BASE_MS + 100, "0.0001")];
        assert!(!window_is_covered(&duplicate_hour, BASE_MS, BASE_MS + 100));
    }

    #[test]
    fn a_dataset_missing_a_settlement_inside_the_window_is_not_full_coverage() {
        // The dataset spans both endpoints, so an endpoint-only span check calls
        // this covered. One settlement inside the window is absent, so the
        // funding total is short by that hour and must not read as complete.
        let temp = TempDir::new();
        let records = [
            record(BASE_MS, "0.0001"),
            // BASE_MS + HOUR_MS is missing.
            record(BASE_MS + 2 * HOUR_MS, "0.0001"),
            record(BASE_MS + 3 * HOUR_MS, "0.0001"),
        ];
        let dataset = dataset_with(&records, temp.path());
        let body = format!(
            r#"{{
              "schema_version": 1,
              "side": "short",
              "size": "1",
              "perp_entry_price": "80000",
              "entry_time_ms": {BASE_MS},
              "exit": {{"perp_price": "80000", "time_ms": {}}}
            }}"#,
            BASE_MS + 2 * HOUR_MS
        );
        let trade = write_trade(&temp, &body);
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");

        let view = evaluate(&dataset, &options, document).expect("evaluates");

        assert!(!view.window_fully_covered);
        // The endpoints are spanned, which is what made the old check pass.
        assert!(view.dataset_first_settlement_ms <= BASE_MS);
        assert!(view.dataset_last_settlement_ms >= BASE_MS + 2 * HOUR_MS);
        // Only one of the two hours the position held through was applied.
        assert_eq!(view.settlements, 1);
        assert_eq!(view.funding, "8");
    }

    #[test]
    fn a_dataset_spanning_both_ends_is_full_coverage() {
        let temp = TempDir::new();
        let records = [
            record(BASE_MS - HOUR_MS, "0.0001"),
            record(BASE_MS, "0.0001"),
            record(BASE_MS + HOUR_MS, "0.0001"),
            record(BASE_MS + 2 * HOUR_MS, "0.0001"),
        ];
        let dataset = dataset_with(&records, temp.path());
        let body = format!(
            r#"{{
              "schema_version": 1,
              "side": "short",
              "size": "1",
              "perp_entry_price": "80000",
              "entry_time_ms": {BASE_MS},
              "exit": {{"perp_price": "80000", "time_ms": {}}}
            }}"#,
            BASE_MS + 2 * HOUR_MS
        );
        let trade = write_trade(&temp, &body);
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");

        let view = evaluate(&dataset, &options, document).expect("evaluates");

        assert!(view.window_fully_covered);
        assert_eq!(view.settlements, 2);
    }

    #[test]
    fn fixed_valuation_overrides_the_entry_price() {
        let temp = TempDir::new();
        let records = [record(BASE_MS + HOUR_MS, "0.0001")];
        let dataset = dataset_with(&records, temp.path());
        let body = format!(
            r#"{{
              "schema_version": 1,
              "side": "short",
              "size": "1",
              "perp_entry_price": "80000",
              "entry_time_ms": {BASE_MS},
              "settlement_valuation": {{"kind": "fixed", "price": "100000"}}
            }}"#
        );
        let trade = write_trade(&temp, &body);
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");

        let view = evaluate(&dataset, &options, document).expect("evaluates");

        assert_eq!(view.settlement_valuation, "fixed");
        assert_eq!(view.valuation_price, "100000");
        assert_eq!(view.funding, "10"); // 100_000 * 0.0001, not 80_000 * 0.0001
    }

    #[test]
    fn json_view_carries_the_stable_contract() {
        let temp = TempDir::new();
        let records = [record(BASE_MS + HOUR_MS, "0.0001")];
        let dataset = dataset_with(&records, temp.path());
        let trade = write_trade(&temp, &hedged_short_trade());
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");
        let view = evaluate(&dataset, &options, document).expect("evaluates");

        let mut buffer = Vec::new();
        write_pnl(&mut buffer, &view, OutputFormat::Json).expect("json writes");
        let json: Value = serde_json::from_slice(&buffer).expect("json parses");

        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["network"], "mainnet");
        assert_eq!(json["venue"], "hyperliquid");
        assert_eq!(json["coin"], "BTC");
        assert_eq!(json["side"], "short");
        assert_eq!(json["settlement_valuation"], "perp-entry-price");
        assert_eq!(json["funding"], "8");
        assert_eq!(json["net"], "8");
        assert!(json["annualized_return"].is_string());
    }

    #[test]
    fn human_view_renders_the_decomposition() {
        let temp = TempDir::new();
        let records = [record(BASE_MS + HOUR_MS, "0.0001")];
        let dataset = dataset_with(&records, temp.path());
        let trade = write_trade(&temp, &hedged_short_trade());
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");
        let view = evaluate(&dataset, &options, document).expect("evaluates");

        let mut buffer = Vec::new();
        write_pnl(&mut buffer, &view, OutputFormat::Human).expect("human writes");
        let rendered = String::from_utf8(buffer).expect("output is utf-8");

        assert!(rendered.contains("BTC-PERP · HYPERLIQUID MAINNET · SHORT CARRY"));
        assert!(rendered.contains("Funding"));
        assert!(rendered.contains("Net"));
        assert!(rendered.contains("spot hedged"));
    }

    #[test]
    fn unsupported_trade_schema_version_is_a_configuration_error() {
        let temp = TempDir::new();
        let body = hedged_short_trade().replace("\"schema_version\": 1", "\"schema_version\": 2");
        let trade = write_trade(&temp, &body);

        let error = TradeDocument::load(&trade).expect_err("unsupported version fails");

        assert_eq!(error.category(), ErrorCategory::Configuration);
    }

    #[test]
    fn unknown_trade_field_is_rejected() {
        let temp = TempDir::new();
        let body = hedged_short_trade().replace(
            "\"side\": \"short\"",
            "\"side\": \"short\", \"leverage\": \"3\"",
        );
        let trade = write_trade(&temp, &body);

        let error = TradeDocument::load(&trade).expect_err("unknown field fails");

        assert_eq!(error.category(), ErrorCategory::Configuration);
    }

    #[test]
    fn exit_before_entry_is_rejected() {
        let temp = TempDir::new();
        let body = format!(
            r#"{{
              "schema_version": 1,
              "side": "short",
              "size": "1",
              "perp_entry_price": "80000",
              "entry_time_ms": {BASE_MS},
              "exit": {{"perp_price": "80000", "time_ms": {}}}
            }}"#,
            BASE_MS - HOUR_MS
        );
        let trade = write_trade(&temp, &body);

        let error = TradeDocument::load(&trade).expect_err("reversed window fails");

        assert_eq!(error.category(), ErrorCategory::Configuration);
    }

    #[test]
    fn empty_dataset_is_partial_data() {
        let temp = TempDir::new();
        let dataset = SettledFundingDataset::new(temp.path());
        let trade = write_trade(&temp, &hedged_short_trade());
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");

        let error = evaluate(&dataset, &options, document).expect_err("empty dataset fails");

        assert_eq!(error.category(), ErrorCategory::PartialData);
    }

    #[test]
    fn invalid_trade_input_surfaces_as_configuration() {
        let temp = TempDir::new();
        let records = [record(BASE_MS + HOUR_MS, "0.0001")];
        let dataset = dataset_with(&records, temp.path());
        let body = hedged_short_trade().replace("\"size\": \"1\"", "\"size\": \"0\"");
        let trade = write_trade(&temp, &body);
        let options = options(&temp, trade);
        let document = TradeDocument::load(&options.trade).expect("document loads");

        let error = evaluate(&dataset, &options, document).expect_err("zero size fails");

        assert_eq!(error.category(), ErrorCategory::Configuration);
    }
}
