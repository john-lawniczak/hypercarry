//! Offline operator setup check; does not approve a release or contact services.
use clap::Parser;
use std::{path::PathBuf, process::ExitCode};
#[derive(Parser)]
#[command(
    about = "Check mainnet setup offline without approval, signing, network calls or state writes."
)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    policy: PathBuf,
    #[arg(long)]
    supervisor: PathBuf,
    #[arg(long)]
    artifacts_dir: PathBuf,
}
fn main() -> ExitCode {
    let args = Args::parse();
    let report = hypercarry_mainnet_services::doctor::check(
        &args.config,
        &args.policy,
        &args.supervisor,
        &args.artifacts_dir,
    );
    match serde_json::to_string_pretty(&report) {
        Ok(json) => println!("{json}"),
        Err(_) => return ExitCode::FAILURE,
    }
    if report.passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
