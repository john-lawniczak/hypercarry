use hypercarry_core::info::Network;
use hypercarry_recorder::{
    MarketConnection, MarketTransport, RawEvent, Recorder, RecorderConfig, RecorderTiming,
    ReplayEvent, ReplayTransport,
    transport::{TransportError, TransportMessage},
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::json;
use std::{
    collections::VecDeque,
    fs::File,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
enum Step {
    Message(TransportMessage),
    Delay(Duration),
}

#[derive(Clone)]
struct ScriptedTransport {
    network: Network,
    scripts: Arc<Mutex<VecDeque<VecDeque<Step>>>>,
    sent: Arc<Mutex<Vec<String>>>,
    closes: Arc<Mutex<u64>>,
}

impl ScriptedTransport {
    fn new(network: Network, scripts: Vec<Vec<Step>>) -> Self {
        Self {
            network,
            scripts: Arc::new(Mutex::new(
                scripts.into_iter().map(VecDeque::from).collect(),
            )),
            sent: Arc::new(Mutex::new(Vec::new())),
            closes: Arc::new(Mutex::new(0)),
        }
    }
}

struct ScriptedConnection {
    steps: VecDeque<Step>,
    sent: Arc<Mutex<Vec<String>>>,
    closes: Arc<Mutex<u64>>,
}

impl MarketTransport for ScriptedTransport {
    type Connection = ScriptedConnection;

    fn network(&self) -> Network {
        self.network
    }

    fn connect(&self) -> impl Future<Output = Result<Self::Connection, TransportError>> + Send {
        let result = self
            .scripts
            .lock()
            .expect("script lock is not poisoned")
            .pop_front()
            .ok_or_else(|| {
                TransportError::WebSocket(tokio_tungstenite::tungstenite::Error::ConnectionClosed)
            })
            .map(|script| ScriptedConnection {
                steps: script,
                sent: Arc::clone(&self.sent),
                closes: Arc::clone(&self.closes),
            });
        std::future::ready(result)
    }
}

impl MarketConnection for ScriptedConnection {
    fn send_text(
        &mut self,
        text: String,
    ) -> impl Future<Output = Result<(), TransportError>> + Send {
        self.sent
            .lock()
            .expect("sent lock is not poisoned")
            .push(text);
        std::future::ready(Ok(()))
    }

    fn send_pong(
        &mut self,
        _payload: Vec<u8>,
    ) -> impl Future<Output = Result<(), TransportError>> + Send {
        std::future::ready(Ok(()))
    }

    async fn next_message(&mut self) -> Result<Option<TransportMessage>, TransportError> {
        match self.steps.front() {
            Some(Step::Message(_)) => {
                let Some(Step::Message(message)) = self.steps.pop_front() else {
                    unreachable!("front was a message")
                };
                Ok(Some(message))
            }
            Some(Step::Delay(duration)) => {
                let duration = *duration;
                tokio::time::sleep(duration).await;
                self.steps.pop_front();
                Ok(None)
            }
            None => Ok(None),
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send {
        let mut closes = self.closes.lock().expect("close lock is not poisoned");
        *closes += 1;
        std::future::ready(Ok(()))
    }
}

fn l2_frame(time: i64) -> String {
    l2_frame_with_bid(time, "100.0")
}

fn l2_frame_with_bid(time: i64, bid_px: &str) -> String {
    json!({
        "channel": "l2Book",
        "data": {
            "coin": "BTC",
            "time": time,
            "levels": [
                [{"px": bid_px, "sz": "2.5", "n": 1}],
                [{"px": "100.1", "sz": "3.5", "n": 2}]
            ]
        }
    })
    .to_string()
}

fn asset_context_frame() -> String {
    json!({
        "channel": "activeAssetCtx",
        "data": {
            "coin": "BTC",
            "ctx": {
                "funding": "0.0001",
                "openInterest": "123.45",
                "oraclePx": "100.0",
                "markPx": "100.05",
                "midPx": "100.025"
            }
        }
    })
    .to_string()
}

fn test_timing() -> RecorderTiming {
    RecorderTiming::new(
        Duration::from_millis(100),
        Duration::from_millis(50),
        Duration::from_millis(100),
        Duration::from_millis(1),
        Duration::from_millis(4),
        8,
    )
    .expect("test timing is valid")
}

#[tokio::test]
async fn recorder_reconnects_and_measures_duplicate_order_stale_and_parse_edges() {
    let temporary = TempDir::new().expect("temporary dataset creates");
    let first = l2_frame(2_000);
    let transport = ScriptedTransport::new(
        Network::Testnet,
        vec![
            vec![
                Step::Message(TransportMessage::Text(first.clone())),
                Step::Message(TransportMessage::Text(first)),
                Step::Message(TransportMessage::Text(l2_frame_with_bid(2_000, "99.9"))),
                Step::Message(TransportMessage::Text(l2_frame(1_000))),
                Step::Message(TransportMessage::Close(Some(
                    "fixture disconnect".to_owned(),
                ))),
            ],
            vec![
                Step::Message(TransportMessage::Text("{malformed".to_owned())),
                Step::Message(TransportMessage::Text(asset_context_frame())),
                Step::Delay(Duration::from_secs(60)),
            ],
        ],
    );
    let config = RecorderConfig::new(Network::Testnet, vec!["BTC".to_owned()], temporary.path())
        .expect("recorder config is valid")
        .with_timing(test_timing());
    let recorder = Recorder::new(config, transport.clone()).expect("networks match");
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    let task = tokio::spawn(async move { recorder.run(cancellation).await });
    tokio::time::sleep(Duration::from_millis(40)).await;
    signal.cancel();
    let diagnostics = task
        .await
        .expect("recorder task joins")
        .expect("fixture recording succeeds");

    assert_eq!(diagnostics.connections, 2);
    assert_eq!(diagnostics.reconnects, 1);
    assert_eq!(diagnostics.raw_frames, 6);
    assert_eq!(diagnostics.normalized_frames, 4);
    assert_eq!(diagnostics.duplicate_frames, 1);
    assert_eq!(diagnostics.out_of_order_frames, 1);
    assert_eq!(diagnostics.parse_errors, 1);
    assert_eq!(diagnostics.stale_frames, 3);
    assert_eq!(diagnostics.dropped_normalized_frames, 0);
    assert_eq!(
        *transport.closes.lock().expect("close lock is not poisoned"),
        1
    );

    let subscriptions = transport.sent.lock().expect("sent lock is not poisoned");
    assert_eq!(subscriptions.len(), 4);
    assert!(subscriptions.iter().all(|message| message.contains("BTC")));
    drop(subscriptions);

    let replay = ReplayTransport::open(diagnostics.raw_capture_path.as_ref())
        .expect("raw session opens")
        .collect::<Result<Vec<_>, _>>()
        .expect("raw session replays");
    assert_eq!(replay.len(), 7);
    assert!(matches!(replay.first(), Some(ReplayEvent::Frame(_))));
    assert!(
        replay
            .iter()
            .any(|event| matches!(event, ReplayEvent::Disconnect(_)))
    );

    let parquet =
        File::open(&diagnostics.normalized_parquet_path).expect("normalized Parquet exists");
    let reader = ParquetRecordBatchReaderBuilder::try_new(parquet)
        .expect("normalized Parquet metadata reads")
        .build()
        .expect("normalized Parquet reader builds");
    let rows: usize = reader
        .map(|batch| batch.expect("normalized batch reads").num_rows())
        .sum();
    assert_eq!(rows, 4);
}

#[tokio::test]
async fn stale_connection_is_captured_and_replayable_before_reconnect() {
    let temporary = TempDir::new().expect("temporary dataset creates");
    let scripts = (0..8)
        .map(|_| vec![Step::Delay(Duration::from_secs(60))])
        .collect();
    let transport = ScriptedTransport::new(Network::Testnet, scripts);
    let timing = RecorderTiming::new(
        Duration::from_millis(50),
        Duration::from_millis(2),
        Duration::from_millis(6),
        Duration::from_millis(1),
        Duration::from_millis(2),
        20,
    )
    .expect("stale test timing is valid");
    let config = RecorderConfig::new(Network::Testnet, vec!["BTC".to_owned()], temporary.path())
        .expect("recorder config is valid")
        .with_timing(timing);
    let recorder = Recorder::new(config, transport).expect("networks match");
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    let task = tokio::spawn(async move { recorder.run(cancellation).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    signal.cancel();
    let diagnostics = task
        .await
        .expect("recorder task joins")
        .expect("stale fixture recording succeeds");

    assert!(diagnostics.heartbeats_sent >= 1);
    assert!(diagnostics.stale_frames >= 1);
    let replay = ReplayTransport::open(diagnostics.raw_capture_path.as_ref())
        .expect("stale raw session opens")
        .collect::<Result<Vec<_>, _>>()
        .expect("stale raw session replays");
    assert!(replay.into_iter().any(|event| {
        matches!(
            event,
            ReplayEvent::Stale(record) if matches!(record.event, RawEvent::Stale { .. })
        )
    }));
}

#[test]
fn replay_accepts_additive_v1_fields_but_rejects_future_schema() {
    let temporary = TempDir::new().expect("temporary replay directory creates");
    let compatible = temporary.path().join("compatible.jsonl");
    std::fs::write(
        &compatible,
        concat!(
            r#"{"schema_version":1,"session_id":"s","network":"testnet","connection_id":1,"sequence":1,"received_at_ms":1,"kind":"frame","payload":"{}","future_optional":"ok"}"#,
            "\n"
        ),
    )
    .expect("compatible fixture writes");
    let compatible = ReplayTransport::open(&compatible)
        .expect("compatible replay opens")
        .collect::<Result<Vec<_>, _>>()
        .expect("additive v1 field is tolerated");
    assert_eq!(compatible.len(), 1);

    let future = temporary.path().join("future.jsonl");
    std::fs::write(
        &future,
        concat!(
            r#"{"schema_version":2,"session_id":"s","network":"testnet","connection_id":1,"sequence":1,"received_at_ms":1,"kind":"frame","payload":"{}"}"#,
            "\n"
        ),
    )
    .expect("future fixture writes");
    let error = ReplayTransport::open(&future)
        .expect("future replay file opens")
        .next()
        .expect("future replay contains one row")
        .expect_err("future schema must be explicit");
    assert!(
        error
            .to_string()
            .contains("schema version 2 is unsupported")
    );
}
