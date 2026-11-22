use hypercarry_notifications::*;
use serde_json::json;
use std::{collections::VecDeque, fs, path::Path};

fn config(path: &Path) -> Config {
    Config {
        schema_version: 1,
        state_dir: path.into(),
        sources: vec![],
        routes: vec![Route {
            id: "sms".into(),
            minimum_severity: Severity::Info,
            kinds: vec![],
            provider: Provider::Twilio {
                account_sid: format!("AC{}", "1".repeat(32)),
                from: "+12025550101".into(),
                to: "+12025550102".into(),
                auth_token_file: path.join("fixture.token"),
            },
        }],
        max_attempts: 3,
        retry_ms: 1000,
        poll_ms: 1000,
        delivery_timeout_ms: 3_600_000,
        sends_per_minute: 10,
        critical_reminder_ms: 60_000,
        max_reminders: 1,
    }
}
fn event(kind: Kind, n: u64) -> NotificationEvent {
    NotificationEvent {
        schema_version: 1,
        id: digest(&("fixture", n)).unwrap(),
        network: "testnet".into(),
        trade_id: "sample".into(),
        coin: "EXAMPLE".into(),
        kind,
        observed_at_ms: 1000,
        remaining: Some("0.4".parse().unwrap()),
    }
}
#[derive(Default)]
struct Fake {
    sends: usize,
    polls: usize,
    outcomes: VecDeque<SendResult>,
    receipts: VecDeque<Receipt>,
}
impl Sender for Fake {
    fn send(&mut self, _: &Route, _: &Delivery) -> SendResult {
        self.sends += 1;
        self.outcomes
            .pop_front()
            .unwrap_or(SendResult::Accepted("fixture".into()))
    }
    fn poll(&mut self, _: &Route, _: &str) -> Receipt {
        self.polls += 1;
        self.receipts.pop_front().unwrap_or(Receipt::Pending)
    }
}
#[test]
fn durable_dedup_receipts_and_ack_are_distinct() {
    let dir = private_tempdir();
    let c = config(dir.path());
    let e = event(Kind::StopResidual, 1);
    let mut fake = Fake::default();
    {
        let mut q = Outbox::open(c.clone()).unwrap();
        q.enqueue(&e, 1000).unwrap();
        q.enqueue(&e, 1000).unwrap();
        q.tick(&mut fake, 1000).unwrap();
    }
    let mut q = Outbox::open(c).unwrap();
    assert_eq!(q.state().deliveries.len(), 1);
    q.acknowledge(&e.id, 1100).unwrap();
    q.tick(&mut fake, 3000).unwrap();
    assert_eq!(fake.sends, 1);
    assert!(matches!(
        q.state().deliveries.values().next().unwrap().status,
        DeliveryStatus::Accepted { .. }
    ));
    fake.receipts.push_back(Receipt::Delivered);
    q.tick(&mut fake, 4000).unwrap();
    assert!(matches!(
        q.state().deliveries.values().next().unwrap().status,
        DeliveryStatus::Delivered { .. }
    ));
    q.tick(&mut fake, 70_000).unwrap();
    assert_eq!(fake.sends, 1);
}
#[test]
fn uncertain_sms_is_not_retried_and_unknown_email_reuses_id() {
    let dir = private_tempdir();
    let mut c = config(dir.path());
    c.max_reminders = 0;
    let mut q = Outbox::open(c).unwrap();
    q.enqueue(&event(Kind::StopResidual, 1), 1000).unwrap();
    let mut fake = Fake {
        outcomes: VecDeque::from([SendResult::Unknown]),
        ..Fake::default()
    };
    q.tick(&mut fake, 1000).unwrap();
    q.tick(&mut fake, 3000).unwrap();
    assert_eq!(fake.sends, 1);
    assert_eq!(
        q.state().deliveries.values().next().unwrap().status,
        DeliveryStatus::Uncertain
    );
    let dir2 = private_tempdir();
    let mut c = config(dir2.path());
    c.routes[0].provider = Provider::Resend {
        from: "alerts@example.com".into(),
        to: "user@example.com".into(),
        api_key_file: dir2.path().join("token"),
    };
    let mut q = Outbox::open(c).unwrap();
    q.enqueue(&event(Kind::EntryFilled, 1), 1000).unwrap();
    let id = q.state().deliveries.keys().next().unwrap().clone();
    fake.outcomes.push_back(SendResult::Unknown);
    q.tick(&mut fake, 1000).unwrap();
    q.tick(&mut fake, 3000).unwrap();
    assert_eq!(q.state().deliveries.keys().next().unwrap(), &id);
    assert_eq!(q.state().deliveries[&id].attempts, 2);
}
#[test]
fn rate_limits_backoff_and_failed_delivery_are_persistent() {
    let dir = private_tempdir();
    let mut c = config(dir.path());
    c.sends_per_minute = 1;
    c.max_reminders = 0;
    let mut q = Outbox::open(c.clone()).unwrap();
    q.enqueue(&event(Kind::EntryFilled, 1), 1000).unwrap();
    let mut fake = Fake {
        outcomes: VecDeque::from([SendResult::Retry]),
        ..Fake::default()
    };
    q.tick(&mut fake, 1000).unwrap();
    q.tick(&mut fake, 3000).unwrap();
    assert_eq!(fake.sends, 1);
    drop(q);
    let mut q = Outbox::open(c).unwrap();
    q.tick(&mut fake, 61_000).unwrap();
    assert_eq!(fake.sends, 2);
    fake.receipts.push_back(Receipt::Failed);
    q.tick(&mut fake, 66_000).unwrap();
    assert!(matches!(
        q.state().deliveries.values().next().unwrap().status,
        DeliveryStatus::Failed { .. }
    ));
}
#[test]
fn journal_cursor_handles_partial_lines_and_refuses_mutation() {
    let dir = private_tempdir();
    let source = dir.path().join("events.jsonl");
    let e = event(Kind::ProtectionMissing, 1);
    let mut record = serde_json::to_vec(&json!({"event":{"notification":e}})).unwrap();
    write_private(&source, &json!({})).unwrap();
    fs::write(&source, &record).unwrap();
    let mut c = config(dir.path());
    c.sources.push(Source {
        id: "recovery".into(),
        path: source.clone(),
    });
    let mut q = Outbox::open(c.clone()).unwrap();
    q.ingest(1000).unwrap();
    assert_eq!(q.state().deliveries.len(), 0);
    record.push(b'\n');
    fs::write(&source, &record).unwrap();
    q.ingest(1000).unwrap();
    drop(q);
    let mut q = Outbox::open(c).unwrap();
    q.ingest(1000).unwrap();
    assert_eq!(q.state().deliveries.len(), 1);
    fs::write(&source, b"{}\n").unwrap();
    assert!(q.ingest(1000).is_err());
}
#[test]
fn reminders_stop_after_resolution_without_forging_human_ack() {
    let dir = private_tempdir();
    let mut q = Outbox::open(config(dir.path())).unwrap();
    let e = event(Kind::StopResidual, 1);
    q.enqueue(&e, 1000).unwrap();
    let mut fake = Fake::default();
    q.tick(&mut fake, 1000).unwrap();
    q.tick(&mut fake, 61_000).unwrap();
    assert_eq!(fake.sends, 2);
    let mut closed = event(Kind::PositionClosed, 2);
    closed.observed_at_ms = 62_000;
    q.enqueue(&closed, 62_000).unwrap();
    assert!(q.state().resolved.contains_key(&e.id));
    assert_eq!(q.state().acknowledged.len(), 0);
}
#[test]
fn changed_routes_and_competing_workers_fail_closed() {
    let dir = private_tempdir();
    let c = config(dir.path());
    let mut q = Outbox::open(c.clone()).unwrap();
    q.enqueue(&event(Kind::EntryFilled, 1), 1000).unwrap();
    assert!(Outbox::open(c.clone()).is_err());
    drop(q);
    let mut c2 = c;
    c2.routes[0].minimum_severity = Severity::Critical;
    assert!(Outbox::open(c2).is_err());
}
#[test]
fn restart_after_reserved_send_does_not_repeat_sms() {
    let dir = private_tempdir();
    let c = config(dir.path());
    {
        let mut q = Outbox::open(c.clone()).unwrap();
        q.enqueue(&event(Kind::EntryFilled, 1), 1000).unwrap();
    }
    let path = dir.path().join("outbox.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&read_private(&path).unwrap()).unwrap();
    let d = value["deliveries"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    d["status"] = json!({"state":"sending"});
    d["attempts"] = json!(1);
    d["first_attempt_ms"] = json!(1000);
    write_private(&path, &value).unwrap();
    let mut q = Outbox::open(c).unwrap();
    let mut fake = Fake::default();
    q.tick(&mut fake, 2000).unwrap();
    assert_eq!(fake.sends, 0);
    assert_eq!(
        q.state().deliveries.values().next().unwrap().status,
        DeliveryStatus::Uncertain
    );
}

fn private_tempdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

#[test]
#[cfg(unix)]
fn unsafe_paths_are_refused_without_following_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = private_tempdir();
    let target = dir.path().join("absent");
    let link = dir.path().join("link");
    symlink(&target, &link).unwrap();
    assert!(write_private(&link, &json!({})).is_err());
    assert!(lock_private(&link).is_err());
    assert!(!target.exists());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Outbox::open(config(dir.path())).is_err());
}

#[test]
fn source_failure_does_not_block_other_sources_or_existing_delivery_after_restart() {
    let dir = private_tempdir();
    let broken = dir.path().join("broken.jsonl");
    let healthy = dir.path().join("healthy.jsonl");
    write_private(&broken, &json!({})).unwrap();
    write_private(&healthy, &json!({})).unwrap();
    let mut record =
        serde_json::to_vec(&json!({"notification": event(Kind::EntryFilled, 2)})).unwrap();
    record.push(b'\n');
    fs::write(&healthy, record).unwrap();
    let mut c = config(dir.path());
    c.max_reminders = 0;
    c.sources = vec![
        Source {
            id: "broken".into(),
            path: broken.clone(),
        },
        Source {
            id: "healthy".into(),
            path: healthy,
        },
    ];
    let mut q = Outbox::open(c.clone()).unwrap();
    q.enqueue(&event(Kind::StopResidual, 1), 1000).unwrap();
    drop(q);
    // Missing source must not prevent opening the queue or delivering either event.
    fs::remove_file(&broken).unwrap();
    let mut q = Outbox::open(c).unwrap();
    let mut fake = Fake::default();
    assert_eq!(q.run_pass(&mut fake, || Ok(1000)).unwrap(), vec!["broken"]);
    assert_eq!(fake.sends, 2);
    assert_eq!(q.state().deliveries.len(), 2);
    // A repaired source is retried at its unchanged cursor, without resending originals.
    write_private(&broken, &json!({})).unwrap();
    fs::write(&broken, b"{}\n").unwrap();
    assert_eq!(q.run_pass(&mut fake, || Ok(1100)).unwrap().len(), 0);
    assert_eq!(fake.sends, 2);
}

#[test]
fn expiry_and_retry_backoff_use_time_after_slow_provider_io() {
    use std::{cell::Cell, rc::Rc};
    struct Slow {
        clock: Rc<Cell<u64>>,
        sends: usize,
    }
    impl Sender for Slow {
        fn send(&mut self, _: &Route, _: &Delivery) -> SendResult {
            self.sends += 1;
            self.clock.set(self.clock.get() + 10_000);
            SendResult::Retry
        }
        fn poll(&mut self, _: &Route, _: &str) -> Receipt {
            unreachable!()
        }
    }
    let dir = private_tempdir();
    let mut c = config(dir.path());
    c.delivery_timeout_ms = 60_000;
    c.max_reminders = 0;
    let mut q = Outbox::open(c).unwrap();
    q.enqueue(&event(Kind::EntryFilled, 1), 1000).unwrap();
    q.enqueue(&event(Kind::EntryFilled, 2), 1000).unwrap();
    let clock = Rc::new(Cell::new(60_000));
    let mut sender = Slow {
        clock: clock.clone(),
        sends: 0,
    };
    q.tick_with_clock(&mut sender, || Ok(clock.get())).unwrap();
    assert_eq!(
        sender.sends, 1,
        "second event expired during first provider request"
    );
    assert_eq!(
        q.state()
            .deliveries
            .values()
            .filter(|d| matches!(&d.status,
        DeliveryStatus::Failed { code } if code == "event_expired"))
            .count(),
        1
    );
    let retry = q
        .state()
        .deliveries
        .values()
        .find(|d| d.attempts == 1)
        .unwrap();
    assert_eq!(retry.next_at_ms, 72_000, "backoff starts after response");
    assert_eq!(retry.first_attempt_ms, Some(60_000));
}

#[test]
fn acknowledgement_supersedes_a_rate_limited_reminder() {
    let dir = private_tempdir();
    let mut c = config(dir.path());
    c.sends_per_minute = 1;
    let mut q = Outbox::open(c).unwrap();
    let e = event(Kind::StopResidual, 1);
    q.enqueue(&e, 1000).unwrap();
    let mut fake = Fake::default();
    q.tick(&mut fake, 60_000).unwrap();
    q.tick(&mut fake, 61_000).unwrap();
    assert_eq!(fake.sends, 1);
    assert!(
        q.state()
            .deliveries
            .values()
            .any(|d| d.generation == 1 && d.status == DeliveryStatus::Queued)
    );
    q.acknowledge(&e.id, 62_000).unwrap();
    q.tick(&mut fake, 63_000).unwrap();
    assert!(
        q.state()
            .deliveries
            .values()
            .any(|d| d.generation == 1 && d.status == DeliveryStatus::Superseded)
    );
    assert_eq!(fake.sends, 1);
}

#[test]
fn full_outbox_still_drains_existing_deliveries_when_reminders_are_due() {
    let dir = private_tempdir();
    let c = config(dir.path());
    let mut q = Outbox::open(c.clone()).unwrap();
    q.enqueue(&event(Kind::StopResidual, 1), 1000).unwrap();
    drop(q);
    // Seed historical terminal deliveries without 10,000 filesystem syncs.
    let path = dir.path().join("outbox.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&read_private(&path).unwrap()).unwrap();
    let deliveries = state["deliveries"].as_object_mut().unwrap();
    let mut historical = deliveries.values().next().unwrap().clone();
    historical["status"] = json!({"state":"displayed"});
    historical["generation"] = json!(1);
    for n in 1..10_000 {
        let id = digest(&("history", n)).unwrap();
        historical["id"] = json!(id);
        deliveries.insert(id, historical.clone());
    }
    write_private(&path, &state).unwrap();
    let mut q = Outbox::open(c).unwrap();
    let mut fake = Fake::default();
    q.tick(&mut fake, 61_000).unwrap();
    assert_eq!(fake.sends, 1);
    assert_eq!(q.state().deliveries.len(), 10_000);
}
