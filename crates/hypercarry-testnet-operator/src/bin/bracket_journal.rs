//! Unsigned bridge to the existing journal-backed bracket engine. No transport.
use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use hypercarry_execution::{
    ExecutionNetwork, FileJournal,
    bracket::{BracketEvent, BracketManager, BracketSpec},
    read_journal,
};
use serde::Deserialize;
use std::{
    io::{self, Read},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    journal: PathBuf,
    /// Permit recording a mainnet lifecycle; provides no signing/submission authority.
    #[arg(long)]
    allow_mainnet_journal: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Reserve,
    Observe,
    Status,
    /// Derive a recovery policy from replay, without rewriting the bracket.
    RecoveryPolicy,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reservation {
    spec: BracketSpec,
    nonce: u64,
}

fn input<T: serde::de::DeserializeOwned>() -> Result<T> {
    let mut bytes = Vec::new();
    io::stdin().lock().take(65_537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65_536, "input exceeds 64 KiB");
    Ok(serde_json::from_slice(&bytes)?)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    // Acquire the existing journal writer lock before reading replay state.
    let mut journal = FileJournal::open(&args.journal)?;
    let manager = match args.command {
        Command::RecoveryPolicy => {
            let manager = BracketManager::replay(&read_journal(&args.journal)?)?;
            let limits = input::<hypercarry_execution::recovery::RecoveryLimits>()?;
            let policy =
                hypercarry_execution::recovery::RecoveryPolicy::from_bracket(&manager, limits)?;
            println!(
                "{}",
                serde_json::json!({"policy":policy,"spec_digest":manager.spec.digest()?})
            );
            return Ok(());
        }
        Command::Reserve => {
            let Reservation { spec, nonce } = input()?;
            ensure!(
                spec.network == ExecutionNetwork::Testnet || args.allow_mainnet_journal,
                "mainnet journal recording requires explicit opt-in"
            );
            let at = i64::try_from(nonce)?;
            ensure!(at <= now && now - at <= 5_000, "stale reservation time");
            let mut manager = BracketManager::create(spec, &mut journal, at)?;
            manager.record(
                &mut journal,
                at,
                BracketEvent::PlanApproved {
                    spec_digest: manager.spec.digest()?,
                    expires_at_ms: at + 10_000,
                },
            )?;
            manager.record(
                &mut journal,
                at,
                BracketEvent::ConfigurationVerified {
                    isolated: true,
                    leverage: 1,
                },
            )?;
            manager.record(&mut journal, at, BracketEvent::EntryReserved { nonce })?;
            manager
        }
        Command::Observe | Command::Status => {
            let mut manager = BracketManager::replay(&read_journal(&args.journal)?)?;
            if matches!(args.command, Command::Observe) {
                let event: BracketEvent = input()?;
                ensure!(
                    matches!(
                        event,
                        BracketEvent::Observed { .. }
                            | BracketEvent::ProtectionRepairReserved { .. }
                    ),
                    "bridge only accepts observations and bounded repair reservations"
                );
                manager.record(&mut journal, now, event)?;
            }
            manager
        }
    };
    println!("{}", serde_json::to_string(&manager)?);
    Ok(())
}
