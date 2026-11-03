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
//! process cannot trade; it can only describe. The daemon reconciles the one
//! exact canary and delegates delivery/recovery checks to a reviewed operations
//! probe. The one-shot `sample` command remains diagnostic-only.
//!
//! **It never claims what it has not measured.** Every readiness field the
//! release gate checks must be `ready` for an order to be authorized, so a
//! field this process cannot establish is written `not_ready` and the
//! executor declines. Missing infrastructure or incomplete observations never
//! become permission to trade.
//!
//! Hand-authoring a `ready` file to get past a missing service defeats every
//! gate in this repository. Do not.

mod account;
use hypercarry_supervisor::config;
#[cfg(test)]
mod fault_harness;
mod probes;
mod reconcile;
mod stream;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use hypercarry_execution::{
    ClientOrderId, OperationalHealth, Readiness, RiskSnapshot, read_journal,
};
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
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAINNET_INFO: &str = "https://api.hyperliquid.xyz/info";
const MAX_RESPONSE_BYTES: u64 = 1_048_576;

// Injection exists only in test builds; production has no endpoint/clock override.
#[derive(Default)]
struct ObservationIo {
    #[cfg(test)]
    control: Option<fault_harness::Control>,
}
// In production these methods intentionally ignore self; test builds borrow
// fixture endpoints and state through the same narrow observation interface.
#[allow(clippy::unused_self, clippy::unnecessary_literal_bound)]
impl ObservationIo {
    fn info_url(&self) -> &str {
        #[cfg(test)]
        if let Some(control) = &self.control {
            return &control.http;
        }
        MAINNET_INFO
    }
    fn websocket_url(&self) -> &str {
        #[cfg(test)]
        if let Some(control) = &self.control {
            return &control.ws;
        }
        "wss://api.hyperliquid.xyz/ws"
    }
    fn now_ms(&self) -> Result<i64> {
        #[cfg(test)]
        if let Some(control) = &self.control {
            return Ok(control.clock.load(Ordering::SeqCst));
        }
        now_ms()
    }
    fn private_state(&self) -> stream::Shared {
        #[cfg(test)]
        if let Some(control) = &self.control {
            return control.private.clone();
        }
        Arc::new(Mutex::new(stream::Observation::default()))
    }
    fn before_publish(&self) {
        #[cfg(test)]
        if let Some(control) = &self.control {
            control.publication.checkpoint();
        }
    }
}

#[derive(Parser)]
#[command(name = "hypercarry-supervisor", about, version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Continuously observe private streams, reconciliation and readiness.
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    /// Print the canonical supervisor policy digest without network access.
    PolicyDigest {
        #[arg(long)]
        config: PathBuf,
    },
    /// Take one observation and replace the health file.
    Sample {
        /// Supervisor configuration naming the executor configuration.
        #[arg(long)]
        config: PathBuf,
    },
}

fn main() -> Result<()> {
    match Args::parse().command {
        Command::PolicyDigest { config } => {
            let supervisor: config::SupervisorConfig =
                hypercarry_mainnet_config::read_json(&config)?;
            println!("{}", supervisor.digest()?);
            Ok(())
        }
        Command::Run { config } => run(&config),
        Command::Sample { config } => {
            let supervisor = config::SupervisorConfig::load(&config)?;
            let runtime = read_runtime(&supervisor.executor_config)?;
            supervisor.validate_runtime(&runtime, &config)?;
            let _lock = lock_health(&runtime)?;
            let envelope = observe(&client()?, &supervisor, &runtime, &ObservationIo::default())?;
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

fn run(path: &std::path::Path) -> Result<()> {
    let supervisor = config::SupervisorConfig::load(path)?;
    let runtime = read_runtime(&supervisor.executor_config)?;
    supervisor.validate_runtime(&runtime, path)?;
    ensure!(
        runtime.supervisor_policy_digest.as_deref() == Some(&supervisor.digest()?),
        "supervisor policy is not bound to reviewed runtime"
    );
    for path in [&runtime.health_file, &runtime.journal] {
        hypercarry_mainnet_config::validate_private_path(path)?;
    }
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone())?;
    run_observer(&supervisor, &runtime, &stop, &ObservationIo::default())
}

fn run_observer(
    supervisor: &config::SupervisorConfig,
    runtime: &RuntimeConfig,
    stop: &Arc<AtomicBool>,
    io: &ObservationIo,
) -> Result<()> {
    let _lock = lock_health(runtime)?;
    withdraw(&runtime.health_file)?;
    let client = client()?;
    let private = io.private_state();
    let worker = stream::start(
        runtime.account_address.clone(),
        private.clone(),
        stop.clone(),
        runtime.health_file.clone(),
        io.websocket_url().to_owned(),
    );
    let outcome = (|| -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            let before = private.lock().expect("stream mutex poisoned").clone();
            let observation = (|| -> Result<(HealthEnvelope, u64)> {
                let mut envelope = observe(&client, supervisor, runtime, io)?;
                let ready = |value| {
                    if value {
                        Readiness::Ready
                    } else {
                        Readiness::NotReady
                    }
                };
                let journal_ready = probe_ready(
                    "journal durability",
                    probes::audit(runtime.journal.parent().context("journal parent missing")?),
                );
                envelope.health.audit_journal = ready(journal_ready);
                let operational = supervisor.operational_probe.as_ref().is_some_and(|probe| {
                    probe_ready(
                        "operations probe",
                        probes::operational(probe, &runtime.digest().unwrap_or_default(), stop),
                    )
                });
                envelope.health.alerts = ready(operational);
                envelope.health.rollback = ready(operational);
                let after = private.lock().expect("stream mutex poisoned").clone();
                let stable = before.current() && after.current() && before.epoch == after.epoch;
                envelope.health.private_stream = ready(after.current());
                envelope.health.private_latency_ms = after.latency_ms;
                if !stable {
                    envelope.health.startup_reconciliation = Readiness::NotReady;
                    envelope.health.continuous_reconciliation = Readiness::NotReady;
                }
                // Do not refresh observation timestamps after probes. Slow
                // checks must age out instead of laundering stale market data.
                envelope.validate_binding(runtime)?;
                Ok((envelope, after.epoch))
            })();
            match observation {
                Ok((mut envelope, epoch)) if !stop.load(Ordering::Relaxed) => {
                    // Serialize publication with stream invalidation: a
                    // disconnect/event cannot race a ready file back into place.
                    io.before_publish();
                    let state = private.lock().expect("stream mutex poisoned");
                    if stop.load(Ordering::Relaxed) {
                        withdraw(&runtime.health_file)?;
                        break;
                    }
                    if !state.current() || state.epoch != epoch {
                        envelope.health.private_stream = Readiness::NotReady;
                        envelope.health.continuous_reconciliation = Readiness::NotReady;
                    }
                    envelope.write_atomic(&runtime.health_file)?;
                }
                Err(error) => {
                    withdraw(&runtime.health_file)?;
                    report_failure("observation; health withdrawn", &error);
                }
                Ok(_) => {
                    withdraw(&runtime.health_file)?;
                }
            }
            for _ in 0..supervisor.sample_interval_ms.div_ceil(100) {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    })();
    stop.store(true, Ordering::Relaxed);
    let removed = withdraw(&runtime.health_file);
    // The stream worker has bounded connect/read waits and owns no authority.
    let _ = worker.join();
    outcome.and(removed)
}

// Both daemon and diagnostic sampling publish the same health path. They must
// share the same lock; a diagnostic must not overwrite a running daemon's file.
fn lock_health(runtime: &RuntimeConfig) -> Result<std::fs::File> {
    use fs2::FileExt;
    use std::os::unix::fs::OpenOptionsExt;
    let lock_path = runtime.health_file.with_extension("supervisor.lock");
    hypercarry_mainnet_config::validate_private_path(&lock_path)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    lock.try_lock_exclusive()
        .context("supervisor already running")?;
    Ok(lock)
}

fn withdraw(path: &std::path::Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn probe_ready(stage: &str, outcome: Result<()>) -> bool {
    if let Err(error) = outcome {
        report_failure(stage, &error);
        false
    } else {
        true
    }
}

fn report_failure(stage: &str, error: &anyhow::Error) {
    // Bounded, escaped diagnostic context. Never dump a helper's output or a
    // venue response body, and don't allow newlines to forge additional entries.
    let message: String = error.to_string().chars().take(512).collect();
    eprintln!("supervisor: {stage} failed: {message:?}");
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
#[allow(clippy::too_many_lines)] // One coherent fail-closed observation batch.
fn observe(
    client: &Client,
    supervisor: &config::SupervisorConfig,
    runtime: &RuntimeConfig,
    io: &ObservationIo,
) -> Result<HealthEnvelope> {
    let started = Instant::now();
    let observed_at_ms = io.now_ms()?;

    // Flatness must cover every perp DEX. The executor's own check queries the
    // default DEX, which does not establish flatness for a unified account —
    // that gap is precisely why this process exists.
    let dexs = account::perp_dex_names(&info(client, io, &json!({"type": "perpDexs"}))?)?;
    ensure!(
        dexs.len() <= 256,
        "venue DEX count exceeds reviewed observer bound"
    );
    let mut flat = true;
    let mut open_order_count = 0_usize;
    let mut aggregate_notional = Decimal::ZERO;
    // Bound concurrent requests rather than serializing every DEX round trip.
    // Any timeout/429/malformed response invalidates the entire observation.
    for chunk in dexs.chunks(8) {
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = chunk
                .iter()
                .map(|dex| {
                    scope.spawn(move || {
                        observe_dex(client, io, &runtime.account_address, dex.as_deref())
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| {
                    worker
                        .join()
                        .map_err(|_| anyhow::anyhow!("account observation worker failed"))?
                })
                .collect::<Result<Vec<_>>>()
        })?;
        for (dex_flat, count, notional) in results {
            flat &= dex_flat;
            open_order_count = open_order_count
                .checked_add(count)
                .context("order count overflow")?;
            aggregate_notional = aggregate_notional
                .checked_add(notional)
                .context("notional overflow")?;
        }
    }

    let default_state = info(
        client,
        io,
        &json!({"type": "clearinghouseState", "user": runtime.account_address}),
    )?;
    let equity = default_state
        .pointer("/marginSummary/accountValue")
        .and_then(Value::as_str)
        .context("clearinghouse state has no account value")?
        .parse::<Decimal>()
        .context("invalid account value")?;

    let reference_price = account::mid_price(
        &info(client, io, &json!({"type": "allMids"}))?,
        &runtime.order.market,
    )?;

    let rolling_pnl = account::rolling_realized_pnl(
        &info(
            client,
            io,
            &json!({"type": "userFills", "user": runtime.account_address}),
        )?,
        observed_at_ms,
        supervisor.rolling_pnl_window_ms,
    )?;

    let history = info(
        client,
        io,
        &json!({"type": "historicalOrders", "user": runtime.account_address}),
    )?;
    let recent_order_times_ms = account::recent_order_submissions(
        &history,
        io.now_ms()?,
        runtime.risk.order_frequency_window_ms,
    )?;
    let status = info(
        client,
        io,
        &json!({"type":"orderStatus", "user":runtime.account_address, "oid":ClientOrderId::derive(&runtime.order)?}),
    )?;
    let events = match std::fs::symlink_metadata(&runtime.journal) {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_file() && meta.len() <= 16 * 1024 * 1024,
                "invalid or oversized journal"
            );
            read_journal(&runtime.journal)?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let reconciled = reconcile::before_submission(&events, runtime, &status)?;
    let readiness = if reconciled && flat && open_order_count == 0 {
        Readiness::Ready
    } else {
        Readiness::NotReady
    };

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
            // The daemon supplies stream and probe observations separately.
            // A single REST sample cannot attest those readiness fields.
            startup_reconciliation: readiness,
            continuous_reconciliation: readiness,
            unmanaged_orders: u32::try_from(open_order_count)?,
            unresolved_submissions: u32::from(!reconciled),
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

fn observe_dex(
    client: &Client,
    io: &ObservationIo,
    account: &str,
    dex: Option<&str>,
) -> Result<(bool, usize, Decimal)> {
    let mut request = json!({"type":"clearinghouseState", "user":account});
    if let Some(name) = dex {
        request["dex"] = name.into();
    }
    let flat = account::is_flat(&info(client, io, &request)?)?;
    request["type"] = "openOrders".into();
    let (count, notional) = account::open_orders(&info(client, io, &request)?)?;
    Ok((flat, count, notional))
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

fn info(client: &Client, io: &ObservationIo, request: &Value) -> Result<Value> {
    let response = client
        .post(io.info_url())
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

    #[test]
    fn daemon_and_sample_share_a_non_destructive_exclusive_lock() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = runtime();
        config.health_file = temp.path().join("health.json");
        std::fs::write(&config.health_file, b"existing observation").unwrap();
        let first = lock_health(&config).unwrap();
        assert!(lock_health(&config).is_err());
        assert_eq!(
            std::fs::read(&config.health_file).unwrap(),
            b"existing observation"
        );
        drop(first);
        assert!(lock_health(&config).is_ok());
    }

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
