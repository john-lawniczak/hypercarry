#![cfg(feature = "mainnet-execution")]
use serde_json::{Value, json};
use std::{fs, process::Command};
#[test]
fn kms_static_config_check_never_invokes_aws_or_reads_signing_stdin() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let cli = temp.path().join("aws");
    // Any attempt to run this stub would fail; static validation must not invoke it.
    fs::write(&cli, "#!/bin/sh\nexit 73\n").unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
    let path = temp.path().join("kms.json");
    let mut config = json!({"schema_version":1,"aws_bin":cli,
        "aws_sha256":hypercarry_mainnet_services::artifact_digest(&cli).unwrap(),
        "region":"us-east-1","kms_key_arn":"arn:aws:kms:us-east-1:123456789012:key/fixture",
        "key_id":"fixture","signer_address":"0x2222222222222222222222222222222222222222","timeout_ms":1000});
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_hypercarry-kms-provider"))
            .arg("--config")
            .arg(&path)
            .arg("--check-config")
            .output()
            .unwrap()
    };
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let result = run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(output["signing_performed"], false);
    assert_eq!(output["kms_access"], "not_checked");
    config["region"] = "us-west-2".into();
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(!run().status.success());
}
#[test]
fn doctor_missing_setup_returns_machine_readable_failure_without_state() {
    let temp = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_hypercarry-doctor"))
        .arg("--config")
        .arg(temp.path().join("config"))
        .arg("--policy")
        .arg(temp.path().join("policy"))
        .arg("--supervisor")
        .arg(temp.path().join("supervisor"))
        .arg("--artifacts-dir")
        .arg(temp.path())
        .output()
        .unwrap();
    assert!(!result.status.success());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(report["trading_authorized"], false);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}
