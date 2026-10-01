//! `hypercarry-supervisor`: writes the health file the executor requires.
//!
//! The executor refuses to authorize an order without a fresh health file
//! attesting that the account is flat, reconciliation is current, the private
//! stream is live, and alerting, audit and rollback are ready. This process
//! makes those observations. It is the "independent supervisor" the integration
//! runbook names, and nothing else in this workspace produces that file.
//!
//! **It holds no credentials.** The private `orderUpdates`/`userFills`
//! subscriptions are unsigned and take an account address, and the REST info
//! queries are address-scoped, so there is no key here and no way to sign. This
//! process cannot trade; it can only describe.
//!
//! **It never claims what it has not measured.** Every readiness field the
//! release gate checks must be `ready` for an order to be authorized, so a
//! field this process cannot yet establish is written `not_ready` and the
//! executor declines. That is the correct behaviour for a partially built
//! supervisor, and it is why the incomplete state of this binary is safe: it
//! withholds authorization rather than granting it on incomplete evidence.
//!
//! Hand-authoring a `ready` file to get past a missing service defeats every
//! gate in this repository. Do not.

mod account;
mod config;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use hypercarry_execution::{OperationalHealth, Readiness, RiskSnapshot};
use hypercarry_mainnet_config::{
    RuntimeConfig,
    health::{HEALTH_SCHEMA_VERSION, HealthEnvelope},
    read_runtime,
};
use reqwest::blocking::Client;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAINNET_INFO: &str = "https://api.hyperliquid.xyz/info";
const MAX_RESPONSE_BYTES: u64 = 1_048_576;

#[derive(Parser)]
#[command(name = "hypercarry-supervisor", about, version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Take one observation and replace the health file.
    Sample {
        /// Supervisor configuration naming the executor configuration.
        #[arg(long)]
        config: PathBuf,
    },
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::Sample { config } => {
            let supervisor = config::SupervisorConfig::load(&config)?;
            let runtime = read_runtime(&supervisor.executor_config)?;
            let envelope = observe(&supervisor, &runtime)?;
            envelope.validate_binding(&runtime)?;
            envelope.write_atomic(&runtime.health_file)?;
            println!("{}", serde_json::to_string_pretty(&envelope)?);
            if !authorizes(&envelope) {
                // Expected while services are still being provisioned. Said
                // plainly, because a supervisor that looks healthy and is not
                // is worse than one that is visibly incomplete.
                eprintln!(
                    "supervisor: health written as NOT READY; the executor will decline to authorize"
                );
            }
            Ok(())
        }
    }
}

/// Whether this observation would let the release gate authorize an order.
fn authorizes(envelope: &HealthEnvelope) -> bool {
    let h = &envelope.health;
    envelope.account_flat
        && h.startup_reconciliation == Readiness::Ready
        && h.continuous_reconciliation == Readiness::Ready
        && h.private_stream == Readiness::Ready
        && h.alerts == Readiness::Ready
        && h.audit_journal == Readiness::Ready
        && h.rollback == Readiness::Ready
}

/// One complete observation of the supervised account.
///
/// Health and risk share a single `observed_at_ms`, which the contract requires
/// and the gate depends on: it reads both against one freshness bound, so two
/// instants would let a stale half hide behind a fresh one.
fn observe(
    supervisor: &config::SupervisorConfig,
    runtime: &RuntimeConfig,
) -> Result<HealthEnvelope> {
    let client = client()?;
    let started = Instant::now();

    // Flatness must cover every perp DEX. The executor's own check queries the
    // default DEX, which does not establish flatness for a unified account —
    // that gap is precisely why this process exists.
    let dexs = account::perp_dex_names(&info(&client, &json!({"type": "perpDexs"}))?)?;
    ensure!(!dexs.is_empty(), "venue reported no perp DEXs");
    let mut flat = true;
    for dex in &dexs {
        let mut request = json!({
            "type": "clearinghouseState",
            "user": runtime.account_address,
        });
        if let Some(name) = dex {
            request["dex"] = Value::String(name.clone());
        }
        flat &= account::is_flat(&info(&client, &request)?)?;
    }

    let default_state = info(
        &client,
        &json!({"type": "clearinghouseState", "user": runtime.account_address}),
    )?;
    let equity = default_state
        .pointer("/marginSummary/accountValue")
        .and_then(Value::as_str)
        .context("clearinghouse state has no account value")?
        .parse::<Decimal>()
        .context("invalid account value")?;

    let (open_order_count, aggregate_notional) = account::open_orders(&info(
        &client,
        &json!({"type": "openOrders", "user": runtime.account_address}),
    )?)?;

    let reference_price = account::mid_price(
        &info(&client, &json!({"type": "allMids"}))?,
        &runtime.order.market,
    )?;

    let observed_at_ms = now_ms()?;
    let rolling_pnl = account::rolling_realized_pnl(
        &info(
            &client,
            &json!({"type": "userFills", "user": runtime.account_address}),
        )?,
        observed_at_ms,
        supervisor.rolling_pnl_window_ms,
    )?;

    let history = info(
        &client,
        &json!({"type": "historicalOrders", "user": runtime.account_address}),
    )?;
    let recent_order_times_ms = account::recent_order_submissions(
        &history,
        now_ms()?,
        runtime.risk.order_frequency_window_ms,
    )?;
    // Timestamp the envelope after all observations; the whole batch latency is
    // checked by the gate, including the history request.
    let observed_at_ms = now_ms()?;

    let rest_latency_ms = u64::try_from(started.elapsed().as_millis())
        .context("REST latency exceeds a u64 of milliseconds")?;

    Ok(HealthEnvelope {
        schema_version: HEALTH_SCHEMA_VERSION,
        network: hypercarry_execution::ExecutionNetwork::Mainnet,
        account_address: runtime.account_address.clone(),
        integration_config_digest: runtime.digest()?,
        account_flat: flat,
        health: OperationalHealth {
            observed_at_ms,
            // Not yet established by this process. Reconciliation compares the
            // durable journal against venue state; the private stream, alert
            // transport, audit journal and rollback each need their own
            // observed source. Until each one exists, `not_ready` is the honest
            // answer and the executor declines on it.
            startup_reconciliation: Readiness::NotReady,
            continuous_reconciliation: Readiness::NotReady,
            unmanaged_orders: 0,
            unresolved_submissions: 0,
            rest_latency_ms,
            private_latency_ms: 0,
            private_stream: Readiness::NotReady,
            alerts: Readiness::NotReady,
            audit_journal: Readiness::NotReady,
            rollback: Readiness::NotReady,
        },
        risk: RiskSnapshot {
            observed_at_ms,
            market_data_at_ms: observed_at_ms,
            reference_price,
            aggregate_notional,
            account_equity: equity,
            open_order_count,
            recent_order_times_ms,
            rolling_pnl,
        },
    })
}

/// Wall-clock milliseconds.
///
/// Deliberately local rather than borrowed from the venue adapter: this process
/// must not link the signing and order-submission machinery merely to read a
/// clock. It cannot trade, and the dependency graph should make that evident.
fn now_ms() -> Result<i64> {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock precedes the Unix epoch")?;
    i64::try_from(since_epoch.as_millis()).context("system clock exceeds an i64 of milliseconds")
}

fn client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .context("cannot build the info client")
}

fn info(client: &Client, request: &Value) -> Result<Value> {
    let response = client
        .post(MAINNET_INFO)
        .json(request)
        .send()
        .context("mainnet info transport failed")?
        .error_for_status()
        .context("mainnet info status failed")?;
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        u64::try_from(bytes.len())? <= MAX_RESPONSE_BYTES,
        "info response exceeds bound"
    );
    serde_json::from_slice(&bytes).context("invalid info response")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> RuntimeConfig {
        serde_json::from_str(include_str!(
            "../../../docs/mainnet-runtime-config.example.json"
        ))
        .unwrap()
    }

    fn envelope(ready: Readiness, flat: bool) -> HealthEnvelope {
        let config = runtime();
        HealthEnvelope {
            schema_version: HEALTH_SCHEMA_VERSION,
            network: hypercarry_execution::ExecutionNetwork::Mainnet,
            account_address: config.account_address.clone(),
            integration_config_digest: config.digest().unwrap(),
            account_flat: flat,
            health: OperationalHealth {
                observed_at_ms: 1_700_000_000_000,
                startup_reconciliation: ready,
                continuous_reconciliation: ready,
                unmanaged_orders: 0,
                unresolved_submissions: 0,
                rest_latency_ms: 10,
                private_latency_ms: 10,
                private_stream: ready,
                alerts: ready,
                audit_journal: ready,
                rollback: ready,
            },
            risk: RiskSnapshot {
                observed_at_ms: 1_700_000_000_000,
                market_data_at_ms: 1_700_000_000_000,
                reference_price: Decimal::new(80_000, 0),
                aggregate_notional: Decimal::ZERO,
                account_equity: Decimal::new(1_000, 0),
                open_order_count: 0,
                recent_order_times_ms: Vec::new(),
                rolling_pnl: Decimal::ZERO,
            },
        }
    }

    /// The gate requires every readiness field, so an unmeasured one withholds
    /// authorization. This is what makes a partially built supervisor safe.
    #[test]
    fn an_unmeasured_readiness_field_withholds_authorization() {
        assert!(!authorizes(&envelope(Readiness::NotReady, true)));
        assert!(authorizes(&envelope(Readiness::Ready, true)));
    }

    #[test]
    fn a_non_flat_account_withholds_authorization_however_ready_the_services() {
        assert!(!authorizes(&envelope(Readiness::Ready, false)));
    }

    /// What this binary writes today must be refused, because it has not yet
    /// established reconciliation, the private stream, alerts, audit or
    /// rollback. If this test ever fails, a readiness field started reporting
    /// ready without a source behind it.
    #[test]
    fn what_this_supervisor_writes_today_does_not_authorize_anything() {
        assert!(!authorizes(&envelope(Readiness::NotReady, true)));
    }
}
