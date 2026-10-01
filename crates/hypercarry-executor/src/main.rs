//! One-shot mainnet canary and recovery CLI. No key loader or order API.
use hypercarry_executor::{
    approval::{self, ApprovalOperation, ConsumedApproval},
    config, safety, signer,
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use config::{Config, RuntimeConfig};
use hypercarry_execution::{
    ComprehensiveRiskPolicy, ExecutionError, ExecutionNetwork, ExplicitMainnetEnable,
    FileDeadManSwitch, FileJournal, FileKillSwitch, InteractiveMainnetConfirmation, Journal,
    JournalEventKind, MAINNET_CONFIRMATION, MainnetReleaseEvidence, MainnetReleaseGate, OrderState,
    RiskDecision, RiskPolicy, ValidatedOrder, read_journal,
};
use hypercarry_hyperliquid::{
    Clock, HyperliquidMainnetConfig, HyperliquidMainnetExecutor, LiveOrder, SystemClock,
};
use safety::SafetySource;
use signer::UnixSocketMainnetSigner;
use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    time::Duration,
};

#[derive(Parser)]
#[command(
    about = "Manual, release-gated Hyperliquid mainnet canary. Requires an external signer and independent health supervisor."
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Print the canonical runtime digest for preparation; does not approve it.
    ConfigDigest {
        #[arg(long)]
        runtime: PathBuf,
    },
    /// Validate a reviewed bundle and build identity offline; does not contact signer or venue.
    Preflight {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        evidence: PathBuf,
    },
    /// Submit one authorized order, cancel remaining size, then reconcile.
    Run {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        evidence: PathBuf,
        #[arg(long)]
        enable_mainnet: bool,
    },
    /// Issue short-lived, one-use MCP consent after interactive confirmation.
    ApproveMcp {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        evidence: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_enum)]
        operation: ApprovalOperation,
        #[arg(long, default_value_t = 60)]
        ttl_seconds: u32,
    },
    /// Consume prepared consent; all release, build and live safety gates remain.
    RunApproved {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        evidence: PathBuf,
        #[arg(long)]
        approval: PathBuf,
        #[arg(long, value_enum)]
        operation: ApprovalOperation,
        #[arg(long)]
        enable_mainnet: bool,
    },
    /// Replay and cancel an existing journal action; never places another order.
    Recover {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        evidence: PathBuf,
    },
}

type Executor = HyperliquidMainnetExecutor<
    UnixSocketMainnetSigner,
    FileJournal,
    SystemClock,
    SafetySource,
    SafetySource,
    FileDeadManSwitch,
>;

fn main() -> std::process::ExitCode {
    match run(Args::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "executor stopped: {error:#}\nPreserve the journal. Resolve any uncertain orders before another run."
            );
            std::process::ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Preflight,
    Run,
    Recover,
}

fn run(args: Args) -> Result<()> {
    match &args.command {
        Command::ApproveMcp {
            config,
            evidence,
            output,
            operation,
            ttl_seconds,
        } => {
            return issue_mcp_approval(config, evidence, output, *operation, *ttl_seconds);
        }
        Command::RunApproved {
            config,
            evidence,
            approval,
            operation,
            enable_mainnet,
        } => {
            ExplicitMainnetEnable::from_cli_flag(*enable_mainnet)?;
            return execute_approved(config, evidence, approval, *operation);
        }
        _ => {}
    }
    let (config_path, evidence_path, mode) = match args.command {
        Command::ConfigDigest { runtime } => {
            let runtime: RuntimeConfig = config::read_json(&runtime)?;
            println!("{}", runtime.digest()?);
            return Ok(());
        }
        Command::Preflight { config, evidence } => (config, evidence, Operation::Preflight),
        Command::Run {
            config,
            evidence,
            enable_mainnet,
        } => {
            ExplicitMainnetEnable::from_cli_flag(enable_mainnet)?;
            (config, evidence, Operation::Run)
        }
        Command::Recover { config, evidence } => (config, evidence, Operation::Recover),
        Command::ApproveMcp { .. } | Command::RunApproved { .. } => unreachable!("handled above"),
    };
    let config = config::load(&config_path)?;
    let evidence = MainnetReleaseEvidence::load(evidence_path)?;
    config::verify_build(&config, &evidence)?;
    let evidence_digest = evidence.evidence_digest()?;
    let gate = MainnetReleaseGate::new(
        config.gate.clone(),
        evidence,
        SafetySource(config.runtime.clone(), config.gate.max_health_age_ms),
        FileDeadManSwitch::new(
            &config.runtime.heartbeat,
            config.runtime.heartbeat_timeout_ms,
        )?,
    )?;
    if mode == Operation::Preflight {
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"network":"mainnet","static_preflight":"passed","runtime_health":"not_checked","source_commit":env!("HYPERCARRY_BUILD_COMMIT")})
        );
        return Ok(());
    }
    execute_session(&config, gate, evidence_digest, mode, None)
}

#[allow(clippy::too_many_lines)] // Keep the ordered, one-shot execution boundary together.
fn execute_session(
    config: &Config,
    gate: MainnetReleaseGate<SafetySource, FileDeadManSwitch>,
    evidence_digest: String,
    mode: Operation,
    consent: Option<ConsumedApproval>,
) -> Result<()> {
    let confirmation = if mode == Operation::Run {
        Some(if let Some(consent) = consent {
            consent.confirmation().to_owned()
        } else {
            confirm(MAINNET_CONFIRMATION)?
        })
    } else {
        if consent.is_none() {
            confirm("RECOVER HYPERCARRY MAINNET ORDERS")?;
        }
        None
    };
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, stop.clone())?;
    }
    let r = &config.runtime;
    let _lease = if mode == Operation::Run {
        hypercarry_executor::lease::LeaseGuard::start(r, stop.clone())?
    } else {
        None
    };
    let signer = UnixSocketMainnetSigner::new(
        r.signer_socket.clone(),
        r.signer_alias.clone(),
        r.signer_address.clone(),
        Duration::from_millis(r.signer_timeout_ms),
    )?;
    // Lock before replay or checking whether the journal has been used.
    let mut journal = FileJournal::open(&r.journal)?;
    let events = read_journal(&r.journal)?;
    let expected_context = JournalEventKind::ExecutionContextBound {
        network: ExecutionNetwork::Mainnet,
        account_id: r.account_address.clone(),
        config_digest: r.digest()?,
        evidence_digest,
        signer_key_id: r.signer_alias.clone(),
    };
    if mode == Operation::Run {
        ensure!(
            events.is_empty(),
            "run requires an unused journal; use recover for an existing action"
        );
        journal.append(
            SystemClock.now_ms()?,
            &r.order.correlation_id,
            expected_context,
        )?;
    } else {
        ensure!(
            events.first().is_some_and(|e| e.event == expected_context),
            "journal belongs to another account/configuration/release"
        );
    }
    let mut executor = Executor::new(
        HyperliquidMainnetConfig::new(
            r.account_address.clone(),
            r.signer_address.clone(),
            r.action_ttl_ms,
        )?,
        signer,
        journal,
        SystemClock,
        SafetySource(r.clone(), config.gate.max_health_age_ms),
        gate,
    )?;
    let mut live = if mode == Operation::Run {
        let policy = ComprehensiveRiskPolicy::new(
            r.risk.clone(),
            SafetySource(r.clone(), config.gate.max_health_age_ms),
            FileKillSwitch::new(&r.kill_switch),
        )?;
        let policy = StopPolicy {
            policy,
            stop: stop.clone(),
        };
        let authorization = executor.authorize(
            &r.order,
            ExplicitMainnetEnable::from_cli_flag(true)?,
            InteractiveMainnetConfirmation::new(
                confirmation.as_deref().context("missing confirmation")?,
            )?,
        )?;
        executor.place(authorization, &policy)?
    } else {
        ensure!(
            events
                .iter()
                .filter(|e| matches!(e.event, JournalEventKind::ExactAction { .. }))
                .count()
                == 1,
            "recovery requires exactly one durable action"
        );
        Executor::recover(r.order.clone(), &events)?
    };
    finish_lifecycle(&mut executor, &mut live, r)?;
    ensure!(
        executor.open_orders()?.is_empty(),
        "final check found default-DEX or spot open orders"
    );
    println!(
        "{}",
        serde_json::json!({"schema_version":1,"network":"mainnet","report":live.report(),
        "client_order_id":live.client_order_id(),"cumulative_filled":live.cumulative_filled().normalize().to_string(),
        "final_default_dex_open_orders":0})
    );
    eprintln!(
        "Final order state: {:?}; cumulative filled quantity: {}. Any filled position remains in the account.",
        live.state(),
        live.cumulative_filled()
    );
    Ok(())
}

fn confirm(phrase: &str) -> Result<String> {
    ensure!(
        io::stdin().is_terminal(),
        "mainnet confirmation requires an interactive terminal"
    );
    eprintln!("Type exactly: {phrase}");
    io::stderr().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim_end_matches(['\r', '\n']).to_owned();
    ensure!(input == phrase, "confirmation mismatch");
    Ok(input)
}

fn finish_lifecycle(
    executor: &mut Executor,
    live: &mut LiveOrder,
    config: &RuntimeConfig,
) -> Result<()> {
    for _ in 0..config.reconciliation_attempts {
        if matches!(
            live.state(),
            OrderState::Filled | OrderState::Cancelled | OrderState::Rejected
        ) {
            return Ok(());
        }
        // Reconcile first, including on restart; a timeout never proves rejection.
        executor.reconcile(live)?;
        if matches!(live.state(), OrderState::Open | OrderState::PartiallyFilled) {
            executor.cancel(live)?;
        }
        std::thread::sleep(Duration::from_millis(config.reconciliation_interval_ms));
    }
    executor.reconcile(live)?;
    ensure!(
        matches!(
            live.state(),
            OrderState::Filled | OrderState::Cancelled | OrderState::Rejected
        ),
        "reconciliation deadline reached with unresolved order"
    );
    Ok(())
}

struct StopPolicy<P> {
    policy: P,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl<P: RiskPolicy> RiskPolicy for StopPolicy<P> {
    fn evaluate(&self, order: &ValidatedOrder) -> Result<RiskDecision, ExecutionError> {
        if self.kill_switch_engaged()? {
            return Ok(RiskDecision::Reject {
                code: "shutdown".into(),
                reason: "shutdown or kill switch requested".into(),
            });
        }
        self.policy.evaluate(order)
    }
    fn kill_switch_engaged(&self) -> Result<bool, ExecutionError> {
        Ok(self.stop.load(std::sync::atomic::Ordering::Relaxed)
            || self.policy.kill_switch_engaged()?)
    }
}

fn reviewed_gate(
    config: &Config,
    evidence: MainnetReleaseEvidence,
) -> Result<MainnetReleaseGate<SafetySource, FileDeadManSwitch>> {
    Ok(MainnetReleaseGate::new(
        config.gate.clone(),
        evidence,
        SafetySource(config.runtime.clone(), config.gate.max_health_age_ms),
        FileDeadManSwitch::new(
            &config.runtime.heartbeat,
            config.runtime.heartbeat_timeout_ms,
        )?,
    )?)
}
fn issue_mcp_approval(
    config_path: &std::path::Path,
    evidence_path: &std::path::Path,
    output: &std::path::Path,
    operation: ApprovalOperation,
    ttl_seconds: u32,
) -> Result<()> {
    let config = config::load(config_path)?;
    let evidence = MainnetReleaseEvidence::load(evidence_path)?;
    config::verify_build(&config, &evidence)?;
    let digest = evidence.evidence_digest()?;
    let _gate = reviewed_gate(&config, evidence)?;
    eprintln!(
        "Approve {:?} for account {}, exact {:?} {} {} at {}, runtime {}",
        operation,
        config.runtime.account_address,
        config.runtime.order.side,
        config.runtime.order.quantity,
        config.runtime.order.market,
        config.runtime.order.limit_price,
        config.runtime.digest()?
    );
    let phrase = confirm(operation.phrase())?;
    approval::issue(
        output,
        &config.runtime,
        &digest,
        operation,
        phrase,
        SystemClock.now_ms()?,
        i64::from(ttl_seconds) * 1000,
    )?;
    println!(
        "{}",
        serde_json::json!({"schema_version":1,"network":"mainnet","approval":"issued","operation":operation,"expires_in_seconds":ttl_seconds})
    );
    Ok(())
}
fn execute_approved(
    config_path: &std::path::Path,
    evidence_path: &std::path::Path,
    approval_path: &std::path::Path,
    operation: ApprovalOperation,
) -> Result<()> {
    let config = config::load(config_path)?;
    let evidence = MainnetReleaseEvidence::load(evidence_path)?;
    config::verify_build(&config, &evidence)?;
    let digest = evidence.evidence_digest()?;
    let gate = reviewed_gate(&config, evidence)?;
    let consent = approval::consume(
        approval_path,
        &config.runtime,
        &digest,
        operation,
        SystemClock.now_ms()?,
    )?;
    let mode = match operation {
        ApprovalOperation::Run => Operation::Run,
        ApprovalOperation::Recover => Operation::Recover,
    };
    execute_session(&config, gate, digest, mode, Some(consent))
}
