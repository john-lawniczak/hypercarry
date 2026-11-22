use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use hypercarry_notifications::{
    Config, NotificationEvent, Outbox, Provider, providers::HttpSender, read_private,
};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
#[command(
    about = "Local notification worker. No wallet access. Network delivery requires --enable-delivery."
)]
struct Args {
    #[arg(long)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Import journal alerts and process the outbox; optional continuous polling.
    Run {
        #[arg(long)]
        once: bool,
        #[arg(long)]
        enable_delivery: bool,
    },
    /// Queue one strict event without sending anything.
    Enqueue {
        #[arg(long)]
        event: PathBuf,
    },
    /// Show durable states without sending anything.
    Status,
    /// Record human acknowledgement and stop reminders (does not forge delivery).
    Ack {
        #[arg(long)]
        event_id: String,
    },
}
fn now() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
fn main() -> Result<()> {
    let args = Args::parse();
    let config: Config = serde_json::from_slice(&read_private(&args.config)?)
        .context("invalid notification config")?;
    let outbound = config
        .routes
        .iter()
        .any(|r| !matches!(r.provider, Provider::Console));
    if let Command::Run {
        enable_delivery, ..
    } = args.command
    {
        ensure!(
            !outbound || enable_delivery,
            "email/SMS routes require explicit --enable-delivery"
        );
    }
    let poll_ms = config.poll_ms;
    match args.command {
        Command::Status => println!(
            "{}",
            serde_json::to_string_pretty(Outbox::open(config)?.state())?
        ),
        Command::Ack { event_id } => Outbox::open(config)?.acknowledge(&event_id, now()?)?,
        Command::Enqueue { event } => {
            let event: NotificationEvent = serde_json::from_slice(&read_private(&event)?)?;
            Outbox::open(config)?.enqueue(&event, now()?)?;
        }
        Command::Run { once, .. } => {
            let mut sender = HttpSender::new()?;
            loop {
                let mut outbox = Outbox::open(config.clone())?;
                let failed = outbox.run_pass(&mut sender, now)?;
                if !failed.is_empty() {
                    eprintln!(
                        "Notification ingestion degraded for sources: {}. Existing deliveries were processed; failed cursors were retained.",
                        failed.join(", ")
                    );
                }
                drop(outbox);
                if once {
                    ensure!(failed.is_empty(), "one or more notification sources failed");
                    break;
                }
                std::thread::sleep(Duration::from_millis(poll_ms));
            }
        }
    }
    Ok(())
}
