//! Local stdio MCP server. Scopes are launch-time capabilities, never tool inputs.
mod config;
mod process;
mod protocol;
mod tools;

use anyhow::{Result, ensure};
use clap::Parser;
use config::{Config, Scope};
use serde_json::json;
use std::{
    collections::BTreeSet,
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Parser)]
#[command(
    about = "Scoped Hypercarry MCP server over stdio; read-only by default",
    version
)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    /// Explicit process capabilities. Local write never implies trade.
    #[arg(long, value_enum, value_delimiter = ',', default_value = "read")]
    scope: Vec<Scope>,
    /// Explicitly permit consumption of separately prepared mainnet approvals.
    #[arg(long)]
    enable_mainnet: bool,
}
fn main() -> Result<()> {
    let args = Args::parse();
    let scopes: BTreeSet<_> = args.scope.into_iter().collect();
    ensure!(
        !scopes.contains(&Scope::Trade) || args.enable_mainnet,
        "trade scope requires explicit --enable-mainnet"
    );
    let config = Config::load(&args.config, &scopes)?;
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, stop.clone())?;
    }
    let mut session = protocol::Session::default();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let mut line = Vec::new();
        let read = (&mut input).take(1_048_577).read_until(b'\n', &mut line)?;
        if read == 0 || stop.load(Ordering::Relaxed) {
            break;
        }
        ensure!(
            line.len() <= 1_048_576 && line.last() == Some(&b'\n'),
            "MCP frame exceeds 1 MiB or is not newline terminated"
        );
        let response = match serde_json::from_slice(&line) {
            Ok(request) => session.handle(&config, &scopes, request, &stop),
            Err(_) => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
            ),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
