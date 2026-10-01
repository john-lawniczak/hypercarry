use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    process::{Command, Stdio},
};
#[test]
fn trade_launch_requires_explicit_mainnet_enable() {
    let output = Command::new(env!("CARGO_BIN_EXE_hypercarry-mcp"))
        .args(["--config", "/nonexistent/config", "--scope", "trade"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "printed to stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("--enable-mainnet")
    );
}
#[test]
fn stdio_handshake_and_local_history_are_real_protocol_messages() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_hypercarry-mcp"))
        .canonicalize()
        .unwrap();
    // No analytics command is invoked for local Parquet history. Pin a real
    // artifact so launch verification is exercised, without requiring a network.
    let bytes = std::fs::read(&binary).unwrap();
    let digest: String = Sha256::digest(bytes)
        .iter()
        .flat_map(|b| {
            let digits = b"0123456789abcdef";
            [
                char::from(digits[usize::from(b >> 4)]),
                char::from(digits[usize::from(b & 15)]),
            ]
        })
        .collect();
    let config = root.join("config.json");
    std::fs::write(&config,serde_json::to_vec(&json!({"schema_version":1,"network":"mainnet","coins":["BTC"],"analytics_bin":binary,"analytics_sha256":digest,"dataset":root.join("data"),"trade_directory":root.join("trades"),"timeout_ms":1000})).unwrap()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_hypercarry-mcp"))
        .arg("--config")
        .arg(&config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_funding_history","arguments":{"coin":"BTC","start_ms":0,"end_ms":3_600_000}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"backfill_funding","arguments":{"coin":"BTC","days":1}}}),
    ] {
        writeln!(input, "{request}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 3);
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        replies[1]["result"]["structuredContent"]["data"]["window_fully_covered"],
        false
    );
    assert_eq!(replies[2]["error"]["code"], -32602);
    assert!(!root.join("data").exists());
}
