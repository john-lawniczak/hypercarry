//! Opt-in public testnet smoke test. Normal CI remains entirely offline.

use hypercarry_core::info::Network;
use hypercarry_recorder::{HyperliquidWebSocketTransport, Recorder, RecorderConfig};
use std::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "opt-in live WebSocket test; requires internet and mutable exchange state"]
async fn testnet_live_recorder_smoke() {
    let temporary = TempDir::new().expect("temporary live dataset creates");
    let config = RecorderConfig::new(Network::Testnet, vec!["BTC".to_owned()], temporary.path())
        .expect("live test config is valid");
    let transport = HyperliquidWebSocketTransport::for_network(Network::Testnet);
    let recorder = Recorder::new(config, transport).expect("live test networks match");
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        signal.cancel();
    });

    let diagnostics = recorder
        .run(cancellation)
        .await
        .expect("testnet public recording succeeds");
    assert!(diagnostics.connections >= 1);
    assert!(diagnostics.raw_frames >= 1);
    assert!(diagnostics.normalized_frames >= 1);
}
