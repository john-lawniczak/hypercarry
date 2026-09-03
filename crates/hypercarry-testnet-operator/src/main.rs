//! `hypercarry-testnet-operator` CLI: preflight, run, and review subcommands.

#![warn(missing_docs)]

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use hypercarry_testnet_operator::{
    OPERATOR_ACKNOWLEDGEMENT, ReviewRequest, preflight, review_session, run_session,
};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "hypercarry-testnet-operator",
    about = "Non-shipping, testnet-only execution evidence harness"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate configuration and local safety boundaries without network I/O.
    Preflight {
        /// Secret-free JSON operator configuration.
        #[arg(long)]
        config: PathBuf,
    },
    /// Place, cancel, and reconcile one exact testnet order.
    Run {
        /// Secret-free JSON operator configuration.
        #[arg(long)]
        config: PathBuf,
        /// Exact acknowledgement of testnet external effects.
        #[arg(long)]
        acknowledgement: String,
    },
    /// Verify immutable session artifacts and write a reviewer-owned attestation.
    Review {
        /// Immutable harness evidence JSON.
        #[arg(long)]
        evidence: PathBuf,
        /// Exact operator config hashed by the harness evidence.
        #[arg(long)]
        config: PathBuf,
        /// Durable lifecycle journal hashed by the harness evidence.
        #[arg(long)]
        journal: PathBuf,
        /// SHA-256 artifact manifest created after the session.
        #[arg(long)]
        manifest: PathBuf,
        /// Independently fetched account-wide final open orders.
        #[arg(long)]
        final_open_orders: PathBuf,
        /// Independently fetched final order status.
        #[arg(long)]
        final_order_status: PathBuf,
        /// Independently fetched final user fills.
        #[arg(long)]
        final_user_fills: PathBuf,
        /// New non-overwriting reviewer attestation path.
        #[arg(long)]
        output: PathBuf,
        /// Bounded non-secret identity for the independent reviewer.
        #[arg(long)]
        reviewer: String,
        /// Exact independent-review acknowledgement.
        #[arg(long)]
        acknowledgement: String,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Preflight { config } => {
            let summary = preflight(&config).context("testnet preflight failed")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&summary)
                    .context("could not serialize preflight summary")?
            );
        }
        Command::Run {
            config,
            acknowledgement,
        } => {
            if acknowledgement != OPERATOR_ACKNOWLEDGEMENT {
                bail!("testnet run requires exact acknowledgement `{OPERATOR_ACKNOWLEDGEMENT}`");
            }
            let outcome = run_session(&config).context("testnet session failed")?;
            println!(
                "{}",
                serde_json::to_string_pretty(outcome.evidence())
                    .context("could not serialize session evidence")?
            );
            if !outcome.is_clean() {
                bail!("testnet session evidence is not clean; keep the kill switch engaged");
            }
        }
        Command::Review {
            evidence,
            config,
            journal,
            manifest,
            final_open_orders,
            final_order_status,
            final_user_fills,
            output,
            reviewer,
            acknowledgement,
        } => {
            let outcome = review_session(&ReviewRequest {
                evidence_path: evidence,
                config_path: config,
                journal_path: journal,
                manifest_path: manifest,
                final_open_orders_path: final_open_orders,
                final_order_status_path: final_order_status,
                final_user_fills_path: final_user_fills,
                output_path: output,
                reviewer,
                acknowledgement,
            })
            .context("independent session review failed")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&outcome)
                    .context("could not serialize review outcome")?
            );
        }
    }
    Ok(())
}
