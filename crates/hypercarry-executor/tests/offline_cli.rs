#![cfg(feature = "mainnet-execution")]

use serde_json::{Value, json};
use std::process::Command;

#[test]
fn run_without_explicit_enable_stops_before_reading_config_or_signing() {
    let output = Command::new(env!("CARGO_BIN_EXE_hypercarry-executor"))
        .args([
            "run",
            "--config",
            "/nonexistent/config",
            "--evidence",
            "/nonexistent/evidence",
        ])
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

#[cfg(unix)]
#[test]
fn static_preflight_rejects_missing_reviewed_sessions_without_creating_journal() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut runtime: Value = serde_json::from_str(include_str!(
        "../../../docs/mainnet-runtime-config.example.json"
    ))
    .unwrap();
    for field in [
        "signer_socket",
        "journal",
        "health_file",
        "heartbeat",
        "kill_switch",
    ] {
        runtime[field] = temp.path().join(field).to_str().unwrap().into();
    }
    let runtime_path = temp.path().join("runtime.json");
    std::fs::write(&runtime_path, serde_json::to_vec(&runtime).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hypercarry-executor"))
        .arg("config-digest")
        .arg("--runtime")
        .arg(&runtime_path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let digest = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    assert_eq!(digest.len(), 64);
    let config = json!({"schema_version":1,"runtime":runtime,"gate":{
        "source_commit":"a".repeat(40),"cargo_lock_digest":"b".repeat(64),"allowed_market":"hyperliquid:BTC",
        "canary_notional_limit":"10","reviewed_max_canary_notional":"10","testnet_key_id":"provider/testnet",
        "config_revision":"fixture-v1","config_review":"fixture/review","max_health_age_ms":1000,
        "max_rest_latency_ms":1000,"max_private_latency_ms":1000,"authorization_ttl_ms":5000,"integration_config_digest":digest
    }});
    // Explicitly unapproved evidence: this fixture must never authorize anything.
    let evidence = json!({"schema_version":2,"testnet_sessions":[],"reviewed_bundle":{
        "source_commit":"a".repeat(40),"cargo_lock_digest":"b".repeat(64),"mainnet_transport_artifact":"fixture/unreviewed",
        "mainnet_signer_key_id":"provider/mainnet-canary","config_digest":"c".repeat(64),
        "dependency_audit":"missing","execution_security_review":"missing","rollback_test":"missing"
    },"release_decision":{"approved":false,"approver":"none","decided_at_ms":0,"evidence_digest":"d".repeat(64)}});
    let config_path = temp.path().join("config.json");
    let evidence_path = temp.path().join("evidence.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::write(&evidence_path, serde_json::to_vec(&evidence).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hypercarry-executor"))
        .arg("preflight")
        .arg("--config")
        .arg(config_path)
        .arg("--evidence")
        .arg(evidence_path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "printed to stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("sessions"), "unexpected error: {error}");
    assert!(!temp.path().join("journal").exists());
}

#[test]
fn approved_execution_still_requires_explicit_enable_before_loading_files() {
    let output = Command::new(env!("CARGO_BIN_EXE_hypercarry-executor"))
        .args([
            "run-approved",
            "--config",
            "/nonexistent/config",
            "--evidence",
            "/nonexistent/evidence",
            "--approval",
            "/nonexistent/approval",
            "--operation",
            "run",
        ])
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
