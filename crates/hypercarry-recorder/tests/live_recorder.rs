//! Opt-in public mainnet/testnet smoke tests. Normal CI remains entirely offline.

use hypercarry_core::info::Network;
use hypercarry_recorder::{HyperliquidWebSocketTransport, Recorder, RecorderConfig};
use std::time::Duration;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

async fn smoke_test_network(network: Network) {
    let temporary = TempDir::new().expect("temporary live dataset creates");
    let config = RecorderConfig::new(network, vec!["BTC".to_owned()], temporary.path())
        .expect("live test config is valid");
    let transport = HyperliquidWebSocketTransport::for_network(network);
    let recorder = Recorder::new(config, transport).expect("live test networks match");
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        signal.cancel();
    });

    let diagnostics = tokio::time::timeout(Duration::from_secs(30), recorder.run(cancellation))
        .await
        .expect("public recording and shutdown finish within 30 seconds")
        .expect("public recording succeeds");
    assert!(diagnostics.connections >= 1);
    assert!(diagnostics.raw_frames >= 1);
    assert!(diagnostics.normalized_frames >= 1);
}

#[tokio::test]
#[ignore = "opt-in live WebSocket test; requires internet and mutable exchange state"]
async fn mainnet_live_recorder_smoke() {
    smoke_test_network(Network::Mainnet).await;
}

#[tokio::test]
#[ignore = "opt-in live WebSocket test; requires internet and mutable exchange state"]
async fn testnet_live_recorder_smoke() {
    smoke_test_network(Network::Testnet).await;
}
