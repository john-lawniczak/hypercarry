use super::*;
use serde_json::json;
use std::sync::atomic::AtomicBool;

fn config() -> Config {
    serde_json::from_value(json!({"schema_version":1,"network":"mainnet","coins":["BTC"],"analytics_bin":"/nonexistent/hypercarry","analytics_sha256":"0".repeat(64),"dataset":"/nonexistent/data","trade_directory":"/nonexistent/trades","timeout_ms":1000})).unwrap()
}
fn ready(session: &mut protocol::Session, config: &Config, scopes: &BTreeSet<Scope>) {
    let result=session.handle(config,scopes,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),&AtomicBool::new(false)).unwrap();
    assert_eq!(result["result"]["protocolVersion"], "2025-11-25");
    assert!(
        session
            .handle(
                config,
                scopes,
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                &AtomicBool::new(false)
            )
            .is_none()
    );
}
#[test]
fn read_scope_cannot_discover_or_call_writes_even_with_forged_arguments() {
    let config = config();
    let scopes = BTreeSet::from([Scope::Read]);
    let mut session = protocol::Session::default();
    ready(&mut session, &config, &scopes);
    assert!(
        !tools::list(&scopes)
            .iter()
            .any(|t| t["name"] == "backfill_funding")
    );
    let response=session.handle(&config,&scopes,json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"backfill_funding","arguments":{"coin":"BTC","days":1,"scope":"write"}}}),&AtomicBool::new(false)).unwrap();
    assert_eq!(response["error"]["code"], -32602);
}
#[test]
fn notifications_do_not_execute_mutations_and_handshake_is_required() {
    let config = config();
    let scopes = BTreeSet::from([Scope::Read, Scope::Write]);
    let mut session = protocol::Session::default();
    let call = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"save_trade","arguments":{}}});
    assert_eq!(
        session
            .handle(&config, &scopes, call.clone(), &AtomicBool::new(false))
            .unwrap()["error"]["code"],
        -32002
    );
    ready(&mut session, &config, &scopes);
    let mut notification = call;
    notification.as_object_mut().unwrap().remove("id");
    assert!(
        session
            .handle(&config, &scopes, notification, &AtomicBool::new(false))
            .is_none()
    );
}
#[test]
fn untrusted_paths_networks_and_coins_cannot_cross_launch_scope() {
    let config = config();
    let scopes = BTreeSet::from([Scope::Read, Scope::Write]);
    for args in [
        json!({"coin":"ETH"}),
        json!({"coin":"BTC","network":"testnet"}),
        json!({"coin":"BTC","dataset":"/etc"}),
    ] {
        assert!(
            tools::call(
                &config,
                &scopes,
                "get_funding_apr",
                args,
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }
    for id in ["../runtime", "/etc/passwd", "a/b", ".", ""] {
        assert!(config.trade_path(id).is_err());
    }
}
#[test]
fn envelopes_preserve_decimal_strings_and_mark_unknown_coverage() {
    let value = tools::envelope(
        &config(),
        "get_funding_apr",
        Ok(json!({"funding_rate":"0.000000000000000001","settlement_time_ms":1000})),
    );
    assert_eq!(value["data"]["funding_rate"], "0.000000000000000001");
    assert_eq!(value["observed_at_ms"], 1000);
    assert!(value["coverage"]["window_fully_covered"].is_null());
    for field in [
        "network",
        "units",
        "coverage",
        "valuation_assumptions",
        "provenance",
    ] {
        assert!(value.get(field).is_some());
    }
    let failed = tools::envelope(
        &config(),
        "get_prediction",
        Err(anyhow::anyhow!("missing capture")),
    );
    assert!(failed["observed_at_ms"].is_null());
    assert!(failed["error"].is_string());
    assert!(
        failed["valuation_assumptions"]
            .to_string()
            .contains("not probability of profit")
    );
}
#[test]
fn advertised_tools_have_strict_schemas_and_honest_write_annotations() {
    for tool in tools::list(&BTreeSet::from([Scope::Read, Scope::Write])) {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert_eq!(tool["outputSchema"]["type"], "object");
        let write = tool["name"] == "backfill_funding" || tool["name"] == "save_trade";
        assert_eq!(tool["annotations"]["readOnlyHint"], !write);
    }
}

#[test]
fn history_reads_real_parquet_and_preserves_gaps_across_pagination() {
    use hypercarry_core::{
        info::Network,
        types::{FundingHistoryEntry, TimestampMs},
    };
    use hypercarry_storage::{
        dataset::SettledFundingDataset,
        settled_funding::{
            IngestionProvenance, RequestWindow, SettledFundingRecord, SourceEndpointClass,
        },
    };
    let temp = tempfile::tempdir().unwrap();
    let mut config = config();
    config.dataset = temp.path().to_owned();
    let hour = 3_600_000;
    let records: Vec<_> = [0, 1, 3, 4]
        .into_iter()
        .map(|i| {
            SettledFundingRecord::from_history_entry(
                Network::Mainnet,
                "hyperliquid",
                FundingHistoryEntry {
                    coin: "BTC".into(),
                    time: TimestampMs::new(i * hour),
                    funding_rate: "0.000001".parse().unwrap(),
                    premium: "0".parse().unwrap(),
                },
                IngestionProvenance::for_current_build(
                    SourceEndpointClass::Official,
                    TimestampMs::new(5 * hour),
                    RequestWindow::new(TimestampMs::new(0), TimestampMs::new(5 * hour)).unwrap(),
                ),
            )
            .unwrap()
        })
        .collect();
    SettledFundingDataset::new(temp.path())
        .commit(&records)
        .unwrap();
    let result = tools::call(
        &config,
        &BTreeSet::from([Scope::Read]),
        "get_funding_history",
        json!({"coin":"BTC","start_ms":0,"end_ms":4*hour,"limit":2}),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result["records"].as_array().unwrap().len(), 2);
    assert_eq!(result["has_more"], true);
    assert_eq!(result["window_fully_covered"], false);
    assert_eq!(result["next_start_ms"], hour + 1);
    // Even if the missing hour is just outside the selected rows, coverage
    // cannot be claimed from an earlier endpoint separated by a gap.
    let result = tools::call(
        &config,
        &BTreeSet::from([Scope::Read]),
        "get_funding_history",
        json!({"coin":"BTC","start_ms":2*hour,"end_ms":4*hour}),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result["window_fully_covered"], false);
}

#[cfg(unix)]
#[test]
fn subprocess_deadline_and_binary_hash_are_enforced() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("fixture");
    std::fs::write(&path, b"#!/bin/sh\nexec /bin/sleep 10\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let digest = process::hash(&std::fs::read(&path).unwrap());
    let started = std::time::Instant::now();
    assert!(process::run(&path, &digest, &[], 100, &AtomicBool::new(false)).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    std::fs::write(&path, b"#!/bin/sh\nprintf '{}\\n'\n").unwrap();
    assert!(process::run(&path, &digest, &[], 100, &AtomicBool::new(false)).is_err());
}

/// Callers annotate a command's result with provenance keys, which `serde_json`
/// only permits on an object. A pinned command emitting an array or a scalar
/// must therefore produce an error here, not a panic after it has already run.
#[cfg(unix)]
#[test]
fn a_non_object_command_result_is_an_error_rather_than_a_panic() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    for body in ["[]", "7", "\"text\"", "null", "{}"] {
        let path = temp.path().join("fixture");
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s' '{body}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let digest = process::hash(&std::fs::read(&path).unwrap());
        let result = process::run(&path, &digest, &[], 5_000, &AtomicBool::new(false));
        if body == "{}" {
            // The annotation every caller performs must not panic on this value.
            let mut value = result.unwrap();
            value["executor_sha256"] = json!("x");
            assert_eq!(value["executor_sha256"], "x");
        } else {
            assert!(result.is_err(), "accepted non-object output {body}");
        }
    }
}

#[test]
fn local_write_never_grants_trading_and_trade_arguments_cannot_replace_order() {
    let config = config();
    let local = BTreeSet::from([Scope::Read, Scope::Write]);
    assert!(
        !tools::list(&local)
            .iter()
            .any(|t| t["name"] == "submit_reviewed_canary")
    );
    assert!(
        tools::call(
            &config,
            &local,
            "submit_reviewed_canary",
            json!({"approval_id":"one"}),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    let trading = BTreeSet::from([Scope::Trade]);
    assert!(
        tools::list(&trading)
            .iter()
            .any(|t| t["name"] == "submit_reviewed_canary")
    );
    for args in [
        json!({"approval_id":"../outside"}),
        json!({"approval_id":"one","price":"1"}),
        json!({"approval_id":"one","order":{}}),
        json!({"approval_id":"one","network":"testnet"}),
    ] {
        assert!(
            tools::call(
                &config,
                &trading,
                "submit_reviewed_canary",
                args,
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }
}
