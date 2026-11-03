//! Unsigned private subscriptions; no signing dependency. Every connection or
//! account event changes the reconciliation epoch, including duplicate events.
use anyhow::{Context, Result, ensure};
use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

#[derive(Debug, Default, Clone)]
pub struct Observation {
    pub epoch: u64,
    pub ready: bool,
    pub pong_at: Option<Instant>,
    pub latency_ms: u64,
}
pub type Shared = Arc<Mutex<Observation>>;

impl Observation {
    pub fn current(&self) -> bool {
        self.ready
            && self
                .pong_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(5))
    }
}

pub fn start(
    account: String,
    shared: Shared,
    stop: Arc<AtomicBool>,
    health_file: std::path::PathBuf,
    endpoint: String,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async {
            while !stop.load(Ordering::Relaxed) {
                {
                    let mut state = shared.lock().expect("stream mutex poisoned");
                    state.epoch = state.epoch.wrapping_add(1);
                    state.ready = false;
                    state.pong_at = None;
                    let _ = crate::withdraw(&health_file);
                }
                if let Err(error) =
                    connected(&account, &shared, &stop, &health_file, &endpoint).await
                {
                    crate::report_failure("private stream", &error);
                }
                {
                    let mut state = shared.lock().expect("stream mutex poisoned");
                    state.ready = false;
                    let _ = crate::withdraw(&health_file);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
    })
}

#[derive(Default)]
struct Subscriptions {
    orders: bool,
    fills: bool,
    snapshot: bool,
}

impl Subscriptions {
    fn accept(&mut self, value: &Value, account: &str) -> Result<bool> {
        match value["channel"].as_str() {
            Some("subscriptionResponse") => {
                let sub = &value["data"]["subscription"];
                ensure!(
                    value["data"]["method"] == "subscribe"
                        && sub["user"]
                            .as_str()
                            .is_some_and(|user| user.eq_ignore_ascii_case(account)),
                    "wrong subscription acknowledgement"
                );
                match sub["type"].as_str() {
                    Some("orderUpdates") => self.orders = true,
                    Some("userFills") => self.fills = true,
                    _ => anyhow::bail!("unexpected subscription"),
                }
                Ok(false)
            }
            Some("orderUpdates") => {
                let orders = value["data"].as_array().context("invalid order updates")?;
                ensure!(
                    orders
                        .iter()
                        .all(|o| o["order"]["oid"].is_u64() && o["status"].is_string()),
                    "invalid private order"
                );
                Ok(true)
            }
            Some("userFills") => {
                let data = &value["data"];
                ensure!(
                    data["user"]
                        .as_str()
                        .is_some_and(|user| user.eq_ignore_ascii_case(account)),
                    "wrong fills account"
                );
                ensure!(data["fills"].is_array(), "invalid fills payload");
                if data["isSnapshot"] == true {
                    self.snapshot = true;
                }
                Ok(true)
            }
            _ => anyhow::bail!("unexpected private channel"),
        }
    }
}

// Bound every write as well as every read. A stalled peer must not keep the
// worker alive indefinitely after STOP or leave an old health file in place.
async fn send<S>(socket: &mut S, message: Message) -> Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    tokio::time::timeout(Duration::from_secs(2), socket.send(message)).await??;
    Ok(())
}

async fn connected(
    account: &str,
    shared: &Shared,
    stop: &AtomicBool,
    health_file: &std::path::Path,
    endpoint: &str,
) -> Result<()> {
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(5),
        connect_async_with_config(
            endpoint,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(1_048_576))
                    .max_frame_size(Some(1_048_576)),
            ),
            false,
        ),
    )
    .await??;
    for kind in ["orderUpdates", "userFills"] {
        send(
            &mut socket,
            Message::Text(
                json!({"method":"subscribe", "subscription":{"type":kind, "user":account}})
                    .to_string()
                    .into(),
            ),
        )
        .await?;
    }
    let mut subscriptions = Subscriptions::default();
    let mut ping = Instant::now();
    let mut pending = true;
    send(
        &mut socket,
        Message::Text(json!({"method":"ping"}).to_string().into()),
    )
    .await?;
    while !stop.load(Ordering::Relaxed) {
        if ping.elapsed() >= Duration::from_secs(2) {
            ensure!(!pending, "private stream pong timed out");
            send(
                &mut socket,
                Message::Text(json!({"method":"ping"}).to_string().into()),
            )
            .await?;
            ping = Instant::now();
            pending = true;
        }
        let message = match tokio::time::timeout(Duration::from_millis(250), socket.next()).await {
            Err(_) => continue,
            Ok(value) => value.context("private stream closed")??,
        };
        match message {
            Message::Text(text) => {
                ensure!(text.len() <= 1_048_576, "private message exceeds bound");
                let value: Value = serde_json::from_str(&text)?;
                if value["channel"] == "pong" {
                    ensure!(pending, "unsolicited pong");
                    let mut state = shared.lock().expect("stream mutex poisoned");
                    state.latency_ms = u64::try_from(ping.elapsed().as_millis())?;
                    state.pong_at = Some(Instant::now());
                    pending = false;
                } else if subscriptions.accept(&value, account)? {
                    let mut state = shared.lock().expect("stream mutex poisoned");
                    state.epoch = state.epoch.wrapping_add(1);
                    crate::withdraw(health_file)?;
                }
                shared.lock().expect("stream mutex poisoned").ready =
                    subscriptions.orders && subscriptions.fills && subscriptions.snapshot;
            }
            Message::Ping(data) => send(&mut socket, Message::Pong(data)).await?,
            Message::Pong(_) => {}
            _ => anyhow::bail!("private stream ended or sent unsupported data"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stalled_stream_write_has_a_deadline() {
        let mut sink = Box::pin(futures_util::sink::unfold((), |(), _: Message| {
            std::future::pending::<Result<(), std::io::Error>>()
        }));
        assert!(
            send(&mut sink, Message::Ping(Vec::new().into()))
                .await
                .is_err()
        );
    }
    #[test]
    fn subscriptions_require_both_acknowledgements_and_account_snapshot() {
        let mut state = Subscriptions::default();
        for kind in ["orderUpdates", "userFills"] {
            assert!(!state.accept(&json!({"channel":"subscriptionResponse","data":{"method":"subscribe","subscription":{"type":kind,"user":"account"}}}), "account").unwrap());
        }
        assert!(!state.snapshot);
        assert!(state.accept(&json!({"channel":"userFills","data":{"user":"other","isSnapshot":true,"fills":[]}}), "account").is_err());
        assert!(state.accept(&json!({"channel":"userFills","data":{"user":"account","isSnapshot":true,"fills":[]}}), "account").unwrap());
        assert!(state.orders && state.fills && state.snapshot);
        assert!(
            state
                .accept(&json!({"channel":"orderUpdates","data":[{}]}), "account")
                .is_err()
        );
    }
    #[test]
    fn quiet_accounts_need_fresh_pongs_and_disconnect_withdraws_readiness() {
        let mut state = Observation {
            ready: true,
            pong_at: Some(Instant::now()),
            ..Observation::default()
        };
        assert!(state.current());
        state.ready = false;
        assert!(!state.current());
        state.ready = true;
        state.pong_at = Instant::now().checked_sub(Duration::from_secs(6));
        assert!(!state.current());
    }
}
