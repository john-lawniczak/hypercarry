use crate::{
    config::OutputFormat,
    error::{CliError, ErrorCategory},
};
use clap::Subcommand;
use hypercarry_core::{
    account::{self, AccountMeta, AccountRequest, AccountSnapshot},
    info::{InfoClient, Network, ReqwestInfoTransport},
    trade_plan::{self, PlanInput, PlanMarket},
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Subcommand)]
pub enum AccountView {
    /// Combined balances, position health and protective orders.
    Status,
    /// Discover every perp DEX and inspect positions and reduce-only exit coverage.
    Portfolio {
        /// Return exit code 14 if any position lacks a full TP/SL or has orphan triggers.
        #[arg(long)]
        require_exits: bool,
        /// Save every read attempt in an existing owner-only directory (Unix).
        #[arg(long)]
        archive_dir: Option<PathBuf>,
    },
    /// Positions and derived isolated health; cross position health is not inferred.
    Positions,
    /// Pending native orders, including TP/SL trigger details.
    Orders,
    /// At most 2,000 recent fills, not a complete historical ledger.
    Fills,
    /// Paginated account funding in an explicit Unix-millisecond window.
    Funding {
        #[arg(long)]
        start_ms: u64,
        #[arg(long)]
        end_ms: Option<u64>,
    },
}
fn error(e: impl std::fmt::Display) -> CliError {
    CliError::new(ErrorCategory::PartialData, e.to_string())
}
fn runtime() -> Result<tokio::runtime::Runtime, CliError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(error)
}
fn client(network: Network) -> Result<InfoClient<ReqwestInfoTransport>, CliError> {
    Ok(InfoClient::new(
        ReqwestInfoTransport::for_network(network).map_err(error)?,
    ))
}
fn print_json(value: &impl serde::Serialize) -> Result<(), CliError> {
    let mut out = io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value).map_err(error)?;
    writeln!(out).map_err(error)
}
pub fn run(
    network: Network,
    address: &str,
    view: &AccountView,
    output: OutputFormat,
) -> Result<(), CliError> {
    account::validate_address(address).map_err(error)?;
    runtime()?.block_on(async {
        let client=client(network)?;
        let started=account::now_ms().map_err(error)?;
        let data=match view {
            AccountView::Portfolio { require_exits, archive_dir } => {
                let result=hypercarry_core::portfolio::snapshot(&client,address).await;
                if let Some(root)=archive_dir {
                    let failure=result.as_ref().err().map(ToString::to_string);
                    let observation=hypercarry_storage::observations::Observation {
                        schema_version:1, network, account:address, started_at_ms:started,
                        completed_at_ms:account::now_ms().map_err(error)?,
                        snapshot:result.as_ref().ok(),
                        exit_coverage_observed:result.as_ref().ok().map(hypercarry_core::portfolio::PortfolioSnapshot::exit_coverage_observed),
                        read_error:failure.as_deref(),
                    };
                    let receipt=hypercarry_storage::observations::save(root,&observation)
                        .map_err(|e|CliError::new(ErrorCategory::Storage,format!("archive failed: {e}; read error: {}",failure.as_deref().unwrap_or("none"))))?;
                    eprintln!("Observation saved: {} (sha256 {})",receipt.path.display(),receipt.sha256);
                    let changes=hypercarry_storage::alerts::record_protection_changes(root,&observation)
                        .map_err(|e|CliError::new(ErrorCategory::Storage,format!("notification journal failed: {e}")))?;
                    for event in &changes {
                        eprintln!("Notification queued: {:?} {} (see --archive-dir notifications.jsonl)",event.kind,event.coin);
                    }
                    hypercarry_storage::alerts::touch_heartbeat(root,network,observation.completed_at_ms)
                        .map_err(|e|CliError::new(ErrorCategory::Storage,format!("heartbeat update failed: {e}")))?;
                }
                let s=result.map_err(error)?;
                if matches!(output,OutputFormat::Human) { human_portfolio(&s)?; } else { print_json(&s)?; }
                if *require_exits && !s.exit_coverage_observed() {
                    return Err(error("exit coverage incomplete or orphan triggers observed; inspect portfolio output"));
                }
                return Ok(());
            },
            AccountView::Status|AccountView::Positions=> {
                let s=account::snapshot(&client,address).await.map_err(error)?;
                if matches!(output,OutputFormat::Human) {return human_snapshot(&s);}
                return print_json(&s);
            },
            AccountView::Orders=>account::read::<_,Vec<account::OpenOrder>>(&client,AccountRequest::Orders {user:address.into()}).await.map(|v|json!(v)).map_err(error)?,
            AccountView::Fills=> {
                let rows=account::read::<_,Vec<Value>>(&client,AccountRequest::Fills {user:address.into()}).await.map_err(error)?;
                account::recent_fills_envelope(&rows)
            },
            AccountView::Funding {start_ms,end_ms}=>json!(account::funding(&client,address,*start_ms,end_ms.unwrap_or(started)).await.map_err(error)?),
        };
        print_json(&json!({"schema_version":1,"network":network,"account":address,"scope":data_scope(view),"started_at_ms":started,"completed_at_ms":account::now_ms().map_err(error)?,"data":data}))
    })
}
fn data_scope(view: &AccountView) -> &'static str {
    match view {
        AccountView::Orders => "native_dex_open_orders_including_venue_returned_spot",
        AccountView::Fills => {
            "venue_returned_recent_fills_including_builder_and_spot; not_full_history"
        }
        AccountView::Funding { .. } => "venue_returned_funding_in_requested_window",
        AccountView::Status | AccountView::Positions => "native_perps_and_shared_spot",
        AccountView::Portfolio { .. } => "all_discovered_perp_dexs_and_shared_spot",
    }
}

fn human_portfolio(s: &hypercarry_core::portfolio::PortfolioSnapshot) -> Result<(), CliError> {
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "{} · {} · {} perp DEXs (sequential reads)",
        s.network,
        s.account,
        s.dexs.len()
    )
    .map_err(error)?;
    writeln!(
        out,
        "Observed {}..{} Unix ms",
        s.started_at_ms, s.completed_at_ms
    )
    .map_err(error)?;
    match s.balances.available_usdc() {
        Ok(v) => writeln!(out, "Available Unified USDC: {v} (not a withdrawal quote)"),
        Err(_) => writeln!(out, "Available Unified USDC: UNKNOWN"),
    }
    .map_err(error)?;
    for dex in &s.dexs {
        for envelope in &dex.positions.asset_positions {
            let p = &envelope.position;
            if p.szi.is_zero() {
                continue;
            }
            let coverage = dex
                .exits
                .iter()
                .find(|e| e.coin == p.coin)
                .expect("coverage for every open position");
            writeln!(
                out,
                "{}: quantity {} · entry {:?} · unrealized P&L {} · liquidation {:?}",
                p.coin, p.szi, p.entry_px, p.unrealized_pnl, p.liquidation_px
            )
            .map_err(error)?;
            writeln!(
                out,
                "  full-size reduce-only SL {:?}; TP {:?}",
                coverage.stop_order_ids, coverage.take_order_ids
            )
            .map_err(error)?;
            for order in dex.orders.iter().filter(|o| o.coin == p.coin) {
                writeln!(
                    out,
                    "  {} {} · size {} · trigger {} · bound {} · reduce-only {}",
                    order.oid,
                    order.order_type,
                    order.sz,
                    order.trigger_px,
                    order.limit_px,
                    order.reduce_only
                )
                .map_err(error)?;
            }
        }
        if !dex.orphan_trigger_ids.is_empty() {
            writeln!(
                out,
                "  {} orphan triggers: {:?}",
                dex.dex, dex.orphan_trigger_ids
            )
            .map_err(error)?;
        }
    }
    writeln!(out, "Exit coverage observed: {}. Triggers do not guarantee fills. Read-only, not continuous monitoring.", s.exit_coverage_observed()).map_err(error)
}
fn human_snapshot(s: &AccountSnapshot) -> Result<(), CliError> {
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "{} · {} · native perps (sequential reads)",
        s.network, s.account
    )
    .map_err(error)?;
    writeln!(
        out,
        "Observed {}..{} Unix ms",
        s.started_at_ms, s.completed_at_ms
    )
    .map_err(error)?;
    match s.balances.available_usdc() {
        Ok(v) => writeln!(out, "Available Unified USDC: {v}"),
        Err(_) => writeln!(out, "Available Unified USDC: UNKNOWN (not reported)"),
    }
    .map_err(error)?;
    for envelope in &s.positions.asset_positions {
        let p = &envelope.position;
        writeln!(
            out,
            "{}: signed quantity {} · entry {:?} · unrealized P&L {} · liquidation {:?}",
            p.coin, p.szi, p.entry_px, p.unrealized_pnl, p.liquidation_px
        )
        .map_err(error)?;
        if let Some((_, Some(h))) = s.health.iter().find(|(coin, _)| *coin == p.coin) {
            writeln!(
                out,
                "  isolated equity {} / maintenance {} = {:.4}; liquidation threshold 1",
                h.isolated_equity, h.maintenance_margin, h.equity_over_maintenance
            )
            .map_err(error)?;
        }
        for o in s.orders.iter().filter(|o| o.coin == p.coin) {
            writeln!(out,"  order {}: {} · side {} · size {} · trigger {} · execution bound {} · reduce-only {}",o.oid,o.order_type,o.side,o.sz,o.trigger_px,o.limit_px,o.reduce_only).map_err(error)?;
        }
    }
    writeln!(
        out,
        "{} native position records; {} open orders. Read-only; no key used.",
        s.positions.asset_positions.len(),
        s.orders.len()
    )
    .map_err(error)
}
/// Independent liveness check for a `portfolio --archive-dir` collector. No
/// network or account access: it only compares the archive directory's
/// heartbeat file modification time against `max_age_ms`, so it detects a
/// crashed, wedged, or never-started collector from the outside rather than
/// relying on the collector to notice its own failure. A stale heartbeat
/// exits nonzero after appending a `HeartbeatLost` alert to the same
/// `notifications.jsonl` journal the collector itself writes to.
pub fn heartbeat_check(
    network: Network,
    archive_dir: &Path,
    max_age_ms: u64,
    output: OutputFormat,
) -> Result<(), CliError> {
    let now = account::now_ms().map_err(error)?;
    let stale = hypercarry_storage::alerts::check_heartbeat(archive_dir, network, max_age_ms, now)
        .map_err(|e| {
            CliError::new(
                ErrorCategory::Storage,
                format!("heartbeat check failed: {e}"),
            )
        })?;
    if matches!(output, OutputFormat::Human) {
        let mut out = io::stdout().lock();
        writeln!(
            out,
            "{network} · collector heartbeat {} · checked {now} ms · bound {max_age_ms} ms",
            if stale { "STALE" } else { "fresh" }
        )
        .map_err(error)?;
    } else {
        print_json(
            &json!({"schema_version":1,"network":network,"checked_at_ms":now,"max_age_ms":max_age_ms,"stale":stale}),
        )?;
    }
    if stale {
        return Err(error(
            "collector heartbeat is stale; observation/notification pipeline may have stopped",
        ));
    }
    Ok(())
}
pub fn calculate() -> Result<(), CliError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Calculation {
        input: PlanInput,
        market: hypercarry_core::trade_plan::PlanMarket,
    }
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take(65_537)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > 65_536 {
        return Err(error("calculation exceeds 64 KiB"));
    }
    let calculation: Calculation = serde_json::from_slice(&bytes).map_err(error)?;
    let plan = hypercarry_core::trade_plan::preview(
        calculation.input,
        calculation.market,
        account::now_ms().map_err(error)?,
    )
    .map_err(error)?;
    print_json(&plan)
}

pub fn preview(path: &Path, output: OutputFormat) -> Result<(), CliError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(error)?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() > 65536 {
        return Err(error("plan input exceeds 64 KiB"));
    }
    let input: PlanInput = serde_json::from_slice(&bytes).map_err(error)?;
    account::validate_address(&input.account).map_err(error)?;
    runtime()?.block_on(async {
        let c=client(input.network)?;
        let started=account::now_ms().map_err(error)?;
        let context:Value=account::read(&c,AccountRequest::Contexts).await.map_err(error)?;
        let meta:AccountMeta=serde_json::from_value(context[0].clone()).map_err(error)?;
        let (asset,market,table)=meta.market(&input.coin).map_err(error)?;
        let mark:Decimal=context[1][asset as usize]["markPx"].as_str().ok_or_else(||error("mark missing"))?.parse().map_err(error)?;
        let balances:account::SpotState=account::read(&c,AccountRequest::Balances {user:input.account.clone()}).await.map_err(error)?;
        let positions:account::PositionState=account::read(&c,AccountRequest::Positions {user:input.account.clone()}).await.map_err(error)?;
        if positions.asset_positions.iter().any(|p|!p.position.szi.is_zero()) {return Err(error("preview currently requires a flat native account; existing positions remain observation-only"));}
        let orders:Vec<account::OpenOrder>=account::read(&c,AccountRequest::Orders {user:input.account.clone()}).await.map_err(error)?;
        if !orders.is_empty() {return Err(error("preview requires no existing native open orders"));}
        let p=trade_plan::preview(input,PlanMarket {asset,size_decimals:market.sz_decimals,max_leverage:market.max_leverage,mark,available_usdc:balances.available_usdc().map_err(error)?,taker_fee_rate:Decimal::new(10,4),observed_at_ms:started,margin_table:table},account::now_ms().map_err(error)?).map_err(error)?;
        if matches!(output,OutputFormat::Human) {
            eprintln!("PREVIEW ONLY — isolated {}x {:?} {} {}; entry bound {}; TP {} / SL {}; conservative fee assumption 0.10%; no order will be sent.",p.input.leverage,p.input.direction,p.quantity,p.input.coin,p.entry_limit,p.input.take_profit,p.input.stop_loss);
        }
        print_json(&p)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_envelopes_do_not_mislabel_builder_fills_as_native_only() {
        assert!(data_scope(&AccountView::Fills).contains("builder_and_spot"));
        assert!(data_scope(&AccountView::Fills).contains("not_full_history"));
        assert!(data_scope(&AccountView::Orders).contains("native_dex"));
        assert_eq!(
            data_scope(&AccountView::Funding {
                start_ms: 1,
                end_ms: Some(2)
            }),
            "venue_returned_funding_in_requested_window"
        );
    }
}
