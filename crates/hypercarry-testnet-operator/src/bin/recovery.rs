//! Bounded local testnet stop-failure recovery host; never mainnet execution.
use anyhow::{Result, ensure};
use clap::Parser;
use hypercarry_testnet_operator::run_recovery;
use std::path::PathBuf;
#[derive(Parser)]
#[command(
    about = "Local TESTNET recovery for an owned bracket; requires a separately scoped signer"
)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    acknowledgement: String,
    #[arg(long)]
    once: bool,
}
fn main() -> Result<()> {
    let args = Args::parse();
    let state = run_recovery(&args.config, &args.acknowledgement, args.once)?;
    println!("{}", serde_json::to_string_pretty(&state)?);
    if state.phase != hypercarry_execution::recovery::RecoveryPhase::Closed {
        eprintln!(
            "Session ended with entries disabled; this is not proof of a closed position. Check the journal and native exits."
        );
    }
    ensure!(
        session_succeeded(state.phase, args.once),
        "recovery session ended without verified closure (or healthy --once observation)"
    );
    Ok(())
}

fn session_succeeded(phase: hypercarry_execution::recovery::RecoveryPhase, once: bool) -> bool {
    use hypercarry_execution::recovery::RecoveryPhase;
    phase == RecoveryPhase::Closed || (once && phase == RecoveryPhase::Monitoring)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercarry_execution::recovery::RecoveryPhase;
    #[test]
    fn unresolved_or_escalated_sessions_are_not_success() {
        for once in [false, true] {
            assert!(session_succeeded(RecoveryPhase::Closed, once));
            for phase in [
                RecoveryPhase::Escalated,
                RecoveryPhase::Recovering,
                RecoveryPhase::Reconciling,
                RecoveryPhase::Grace,
            ] {
                assert!(!session_succeeded(phase, once));
            }
        }
        assert!(session_succeeded(RecoveryPhase::Monitoring, true));
        assert!(!session_succeeded(RecoveryPhase::Monitoring, false));
    }
}
