use anyhow::{Context, Result, ensure};
use clap::Parser;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Parser)]
#[command(
    about = "Mainnet signer service. Requires reviewed evidence and an external prehash signing backend."
)]
struct Args {
    #[arg(long, required_unless_present = "policy_digest")]
    config: Option<PathBuf>,
    #[arg(long)]
    policy: PathBuf,
    #[arg(long, required_unless_present = "policy_digest")]
    evidence: Option<PathBuf>,
    #[arg(long)]
    policy_digest: bool,
    #[arg(long)]
    enable_mainnet: bool,
}
fn main() -> Result<()> {
    let args = Args::parse();
    if args.policy_digest {
        let policy: hypercarry_mainnet_services::policy::ServicePolicy =
            hypercarry_mainnet_config::read_json(&args.policy)?;
        println!("{}", hypercarry_mainnet_services::digest(&policy)?);
        return Ok(());
    }
    ensure!(args.enable_mainnet, "--enable-mainnet is required");
    let (config, policy, gate, digest) = hypercarry_mainnet_services::load(
        &args.config.context("missing config")?,
        &args.policy,
        &args.evidence.context("missing evidence")?,
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, stop.clone())?;
    }
    hypercarry_mainnet_services::provider::serve(&config, &policy, &gate, &digest, &stop)
}
