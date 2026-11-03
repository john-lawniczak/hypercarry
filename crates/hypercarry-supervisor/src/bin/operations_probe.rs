//! Portable receipt-backed probe; never sends messages, trades or writes receipts.
use anyhow::Result;
use clap::Parser;
use hypercarry_mainnet_config::read_json;
use hypercarry_supervisor::operations::{Config, observe};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
#[command(
    about = "Verify fresh monitoring receipts and pinned recovery artifacts; no trading authority."
)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    challenge: String,
    #[arg(long)]
    integration_config_digest: String,
}
fn main() -> Result<()> {
    let args = Args::parse();
    let config: Config = read_json(&args.config)?;
    let observation = observe(
        &config,
        &args.challenge,
        &args.integration_config_digest,
        || {
            Ok(i64::try_from(
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
            )?)
        },
    )?;
    println!("{}", serde_json::to_string(&observation)?);
    Ok(())
}
