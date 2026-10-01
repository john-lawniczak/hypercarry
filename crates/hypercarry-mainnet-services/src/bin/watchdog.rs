use anyhow::{Result, ensure};
use clap::Parser;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};
#[derive(Parser)]
#[command(
    about = "Independent mainnet watchdog with venue cancellation and bounded reduce-only recovery"
)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    policy: PathBuf,
    #[arg(long)]
    evidence: PathBuf,
    #[arg(long)]
    enable_mainnet: bool,
    /// Explicitly resume an emergency. Never resumes ordinary order placement.
    #[arg(long)]
    recover: bool,
}
fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(args.enable_mainnet, "--enable-mainnet is required");
    let (config, policy, _, _) =
        hypercarry_mainnet_services::load(&args.config, &args.policy, &args.evidence)?;
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, stop.clone())?;
    }
    hypercarry_mainnet_services::watchdog::run(&config.runtime, &policy, &stop, args.recover)
}
