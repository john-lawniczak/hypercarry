use super::*;
use hypercarry_core::{account::PositionState, portfolio::DexSnapshot};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, time::SystemTime};

const ACCOUNT: &str = "0x1111111111111111111111111111111111111111";

fn dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
fn positions() -> PositionState {
    serde_json::from_value(json!({"assetPositions":[],"time":1,"crossMarginSummary":{},"crossMaintenanceMarginUsed":"0"}))
        .unwrap()
}
fn exit(coin: &str, protected: bool) -> hypercarry_core::portfolio::ExitCoverage {
    hypercarry_core::portfolio::ExitCoverage {
        coin: coin.into(),
        quantity: Decimal::ONE,
        stop_order_ids: if protected { vec![1] } else { vec![] },
        take_order_ids: if protected { vec![2] } else { vec![] },
    }
}
fn snapshot(
    exits: Vec<hypercarry_core::portfolio::ExitCoverage>,
    completed_at_ms: u64,
) -> hypercarry_core::portfolio::PortfolioSnapshot {
    hypercarry_core::portfolio::PortfolioSnapshot {
        schema_version: 2,
        network: Network::Testnet,
        account: ACCOUNT.into(),
        scope: "fixture".into(),
        started_at_ms: completed_at_ms - 1,
        completed_at_ms,
        balances: serde_json::from_value(json!({"balances":[]})).unwrap(),
        dexs: vec![DexSnapshot {
            dex: String::new(),
            started_at_ms: completed_at_ms - 1,
            completed_at_ms,
            positions_before: positions(),
            positions: positions(),
            orders: vec![],
            exits,
            orphan_trigger_ids: vec![],
        }],
    }
}
fn ok(snapshot: &hypercarry_core::portfolio::PortfolioSnapshot, coverage: bool) -> Observation<'_> {
    Observation {
        schema_version: 1,
        network: Network::Testnet,
        account: ACCOUNT,
        started_at_ms: snapshot.started_at_ms,
        completed_at_ms: snapshot.completed_at_ms,
        snapshot: Some(snapshot),
        exit_coverage_observed: Some(coverage),
        read_error: None,
    }
}
fn failed(completed_at_ms: u64) -> Observation<'static> {
    Observation {
        schema_version: 1,
        network: Network::Testnet,
        account: ACCOUNT,
        started_at_ms: completed_at_ms - 1,
        completed_at_ms,
        snapshot: None,
        exit_coverage_observed: None,
        read_error: Some("fixture outage"),
    }
}

#[test]
fn missing_protection_alerts_once_then_verifies_and_closes() {
    let dir = dir();
    let missing = snapshot(vec![exit("BTC", false)], 1_000);
    let obs = ok(&missing, false);
    let events = record_protection_changes(dir.path(), &obs).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, Kind::ProtectionMissing);
    assert_eq!(events[0].coin, "BTC");

    // A repeated poll with the same unresolved condition must not re-alert.
    let repeat = record_protection_changes(dir.path(), &obs).unwrap();
    assert!(repeat.is_empty(), "unexpected events: {repeat:?}");

    let protected = snapshot(vec![exit("BTC", true)], 2_000);
    let verified = ok(&protected, true);
    let events = record_protection_changes(dir.path(), &verified).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, Kind::ProtectionVerified);

    let closed = snapshot(vec![], 3_000);
    let empty = ok(&closed, true);
    let events = record_protection_changes(dir.path(), &empty).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, Kind::PositionClosed);
    assert_eq!(events[0].coin, "BTC");
    assert_eq!(events[0].remaining, Some(Decimal::ZERO));

    let journal = std::fs::read_to_string(dir.path().join("notifications.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 3);
}

#[test]
fn read_failure_alerts_once_and_rearms_after_recovery() {
    let dir = dir();
    let events = record_protection_changes(dir.path(), &failed(1_000)).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, Kind::StateUnknown);
    assert_eq!(events[0].coin, "__account__");

    let repeat = record_protection_changes(dir.path(), &failed(1_500)).unwrap();
    assert!(repeat.is_empty(), "unexpected events: {repeat:?}");

    let empty = snapshot(vec![], 2_000);
    let recovered = ok(&empty, true);
    assert!(
        record_protection_changes(dir.path(), &recovered)
            .unwrap()
            .is_empty(),
        "recovery clears tracked state silently"
    );

    let events = record_protection_changes(dir.path(), &failed(3_000)).unwrap();
    assert_eq!(events.len(), 1, "a fresh failure must alert again");
    assert_eq!(events[0].kind, Kind::StateUnknown);
}

#[test]
fn an_observation_must_have_exactly_one_of_snapshot_or_read_error() {
    let dir = dir();
    let missing = snapshot(vec![], 1_000);
    let both = Observation {
        read_error: Some("inconsistent"),
        ..ok(&missing, true)
    };
    assert!(record_protection_changes(dir.path(), &both).is_err());
    let neither = Observation {
        snapshot: None,
        ..ok(&missing, true)
    };
    assert!(record_protection_changes(dir.path(), &neither).is_err());
}

fn unix_ms_now() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

#[test]
fn heartbeat_staleness_is_judged_by_file_mtime_and_alerts_exactly_once() {
    let dir = dir();
    let now = unix_ms_now();
    touch_heartbeat(dir.path(), Network::Testnet, now).unwrap();
    assert!(!check_heartbeat(dir.path(), Network::Testnet, 60_000, now + 1_000).unwrap());
    assert!(check_heartbeat(dir.path(), Network::Testnet, 60_000, now + 120_000).unwrap());
    let journal = std::fs::read_to_string(dir.path().join("notifications.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 1);
    assert!(journal.contains("heartbeat_lost"));

    // Still stale on the next check: must not append a second alert.
    assert!(check_heartbeat(dir.path(), Network::Testnet, 60_000, now + 200_000).unwrap());
    let journal = std::fs::read_to_string(dir.path().join("notifications.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 1);

    // A collector that was never run at all is reported stale, not panicked on.
    let never_run = dir_without_heartbeat();
    assert!(check_heartbeat(never_run.path(), Network::Testnet, 60_000, now).unwrap());
}
fn dir_without_heartbeat() -> tempfile::TempDir {
    dir()
}
