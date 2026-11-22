//! Demonstrates that a real `hypercarry-notifications` Outbox ingests the
//! `alerts` journal and delivers/resolves events through its unchanged
//! pipeline. No real provider is used; `Provider::Console` and a fake
//! in-test `Sender` stand in, matching how this workspace always exercises
//! delivery without contacting a real email/SMS provider.
use hypercarry_core::{
    account::PositionState,
    info::Network,
    portfolio::{DexSnapshot, ExitCoverage, PortfolioSnapshot},
};
use hypercarry_notifications::{
    Config, Delivery, DeliveryStatus, Kind, Outbox, Provider, Receipt, Route, SendResult, Sender,
    Severity, Source,
};
use hypercarry_storage::{
    alerts,
    observations::{self, Observation},
};
use rust_decimal::Decimal;
use std::os::unix::fs::PermissionsExt;

const ACCOUNT: &str = "0x1111111111111111111111111111111111111111";

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
fn positions() -> PositionState {
    serde_json::from_value(serde_json::json!({"assetPositions":[],"time":1,"crossMarginSummary":{},"crossMaintenanceMarginUsed":"0"}))
        .unwrap()
}
fn snapshot(exits: Vec<ExitCoverage>, completed_at_ms: u64) -> PortfolioSnapshot {
    PortfolioSnapshot {
        schema_version: 2,
        network: Network::Testnet,
        account: ACCOUNT.into(),
        scope: "fixture".into(),
        started_at_ms: completed_at_ms - 1,
        completed_at_ms,
        balances: serde_json::from_value(serde_json::json!({"balances":[]})).unwrap(),
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
fn exit(protected: bool) -> ExitCoverage {
    ExitCoverage {
        coin: "BTC".into(),
        quantity: Decimal::ONE,
        stop_order_ids: if protected { vec![1] } else { vec![] },
        take_order_ids: if protected { vec![2] } else { vec![] },
    }
}
fn observation(snapshot: &PortfolioSnapshot, coverage: bool) -> Observation<'_> {
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
fn outbox_config(dir: &std::path::Path) -> Config {
    Config {
        schema_version: 1,
        state_dir: dir.into(),
        sources: vec![Source {
            id: "portfolio".into(),
            path: dir.join("notifications.jsonl"),
        }],
        routes: vec![Route {
            id: "terminal".into(),
            minimum_severity: Severity::Critical,
            kinds: vec![],
            provider: Provider::Console,
        }],
        max_attempts: 2,
        retry_ms: 1_000,
        poll_ms: 1_000,
        delivery_timeout_ms: 3_600_000,
        sends_per_minute: 10,
        critical_reminder_ms: 3_600_000,
        max_reminders: 0,
    }
}

/// A terminal-display `Sender`, equivalent in behavior to the real
/// `Provider::Console` branch, without pulling in the optional `delivery`
/// feature's `reqwest` dependency just to exercise that one arm.
struct Terminal(Vec<String>);
impl Sender for Terminal {
    fn send(&mut self, _route: &Route, delivery: &Delivery) -> SendResult {
        self.0.push(delivery.event.message());
        SendResult::Displayed
    }
    fn poll(&mut self, _route: &Route, _provider_id: &str) -> Receipt {
        Receipt::Unknown
    }
}

#[test]
fn protection_missing_alert_is_ingested_delivered_and_resolved_on_recovery() {
    let archive = private_dir();
    let missing = snapshot(vec![exit(false)], 1_000);
    let observation_missing = observation(&missing, false);
    observations::save(archive.path(), &observation_missing).unwrap();
    let emitted = alerts::record_protection_changes(archive.path(), &observation_missing).unwrap();
    assert_eq!(emitted.len(), 1);
    assert_eq!(emitted[0].kind, Kind::ProtectionMissing);
    let missing_id = emitted[0].id.clone();

    let mut outbox = Outbox::open(outbox_config(archive.path())).unwrap();
    outbox.ingest(2_000).unwrap();
    assert_eq!(outbox.state().deliveries.len(), 1, "alert must be queued");

    let mut terminal = Terminal(vec![]);
    outbox.tick(&mut terminal, 2_000).unwrap();
    assert_eq!(terminal.0.len(), 1, "the alert must actually be sent");
    assert!(terminal.0[0].contains("ProtectionMissing"));
    assert!(
        outbox
            .state()
            .deliveries
            .values()
            .any(|d| d.status == DeliveryStatus::Displayed),
        "delivery must reach a terminal displayed state"
    );

    // The position regains full reduce-only coverage: the new event must
    // resolve the original alert through the outbox's existing matching
    // logic, with no code change required in hypercarry-notifications.
    let protected = snapshot(vec![exit(true)], 3_000);
    let observation_protected = observation(&protected, true);
    let resolved =
        alerts::record_protection_changes(archive.path(), &observation_protected).unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].kind, Kind::ProtectionVerified);

    outbox.ingest(4_000).unwrap();
    assert!(
        outbox.state().resolved.contains_key(&missing_id),
        "recovery must resolve the original missing-protection alert"
    );
}

#[test]
fn stale_collector_heartbeat_is_delivered_as_a_critical_alert() {
    let archive = private_dir();
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    alerts::touch_heartbeat(archive.path(), Network::Testnet, now).unwrap();
    assert!(
        alerts::check_heartbeat(archive.path(), Network::Testnet, 60_000, now + 10 * 60_000)
            .unwrap(),
        "heartbeat older than its bound must be reported stale"
    );

    let mut outbox = Outbox::open(outbox_config(archive.path())).unwrap();
    outbox.ingest(now + 10 * 60_000 + 1).unwrap();
    assert_eq!(outbox.state().deliveries.len(), 1);
    let mut terminal = Terminal(vec![]);
    outbox.tick(&mut terminal, now + 10 * 60_000 + 1).unwrap();
    assert_eq!(terminal.0.len(), 1);
    assert!(terminal.0[0].contains("HeartbeatLost"));
}
