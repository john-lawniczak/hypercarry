use hypercarry_execution::{
    ExecutionError, ExecutionNetwork, InMemoryJournal, Journal, JournalEvent, JournalEventKind,
    Side, bracket::*, recovery::*,
};
use rust_decimal::Decimal;
fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}
fn fixture() -> (RecoveryManager, InMemoryJournal, Fake) {
    let spec = BracketSpec {
        trade_id: "owned-fixture".into(),
        plan_digest: "ab".repeat(32),
        network: ExecutionNetwork::Testnet,
        account: format!("0x{}", "12".repeat(20)),
        coin: "EXAMPLE".into(),
        entry_side: Side::Sell,
        quantity: d("1.1"),
        tp: d("70"),
        sl: d("95"),
        entry_cloid: format!("0x{}", "11".repeat(16)),
        tp_cloid: format!("0x{}", "22".repeat(16)),
        sl_cloid: format!("0x{}", "33".repeat(16)),
    };
    let mut bj = InMemoryJournal::default();
    let mut b = BracketManager::create(spec.clone(), &mut bj, 1000).unwrap();
    b.record(
        &mut bj,
        1001,
        BracketEvent::PlanApproved {
            spec_digest: spec.digest().unwrap(),
            expires_at_ms: 10_000,
        },
    )
    .unwrap();
    b.record(
        &mut bj,
        1002,
        BracketEvent::ConfigurationVerified {
            isolated: true,
            leverage: 1,
        },
    )
    .unwrap();
    b.record(&mut bj, 1003, BracketEvent::EntryReserved { nonce: 1003 })
        .unwrap();
    let limits = RecoveryLimits {
        size_step: d("0.01"),
        price_tick: d("0.01"),
        min_price: d("50"),
        max_price: d("100"),
        slippage_bps: 50,
        stop_grace_ms: 0,
        max_snapshot_age_ms: 1000,
        max_close_attempts: 3,
        max_cancel_attempts: 4,
        retry_ms: 1000,
        recovery_timeout_ms: 30_000,
        action_ttl_ms: 1000,
    };
    let mut j = InMemoryJournal::default();
    let m = RecoveryManager::from_bracket(&b, limits, &mut j, 2000).unwrap();
    let exit = |tp| ExitObservation {
        cloid: if tp {
            spec.tp_cloid.clone()
        } else {
            spec.sl_cloid.clone()
        },
        coin: spec.coin.clone(),
        side: Side::Buy,
        quantity: d("1.1"),
        trigger: if tp { d("70") } else { d("95") },
        reduce_only: true,
        is_trigger: true,
        is_take_profit: tp,
    };
    let o = RecoverySnapshot {
        network: spec.network,
        account: spec.account.clone(),
        coin: spec.coin.clone(),
        started_at_ms: 2000,
        completed_at_ms: 2000,
        complete: true,
        agent_authorized: true,
        entry: EntryOutcome::Terminal,
        signed_position: d("-1.1"),
        mark: d("96"),
        bid: d("95.99"),
        ask: d("96"),
        tp: Some(exit(true)),
        sl: Some(exit(false)),
        stop_fired: true,
        unmanaged_orders: 0,
        pending: None,
    };
    (
        m,
        j,
        Fake {
            now: 2000,
            o,
            actions: vec![],
            outcome: SubmitOutcome::Accepted,
            fail_read: false,
            read_delay_ms: 0,
            fail_freeze: false,
            freezes: 0,
        },
    )
}
struct Fake {
    now: u64,
    o: RecoverySnapshot,
    actions: Vec<ReservedAction>,
    outcome: SubmitOutcome,
    fail_read: bool,
    read_delay_ms: u64,
    fail_freeze: bool,
    freezes: usize,
}
impl Fake {
    fn at(&mut self, now: u64) {
        self.now = now;
        self.o.started_at_ms = now;
        self.o.completed_at_ms = now;
    }
    fn terminal(&mut self) {
        self.o.pending = Some(OrderEvidence {
            cloid: self.actions.last().unwrap().action.cloid().into(),
            state: ObservedOrderState::Terminal,
        });
    }
}
impl RecoveryVenue for Fake {
    fn freeze_entries(&mut self) -> Result<(), ExecutionError> {
        self.freezes += 1;
        if self.fail_freeze {
            Err(ExecutionError::Policy("gate failed".into()))
        } else {
            Ok(())
        }
    }
    fn now_ms(&self) -> Result<u64, ExecutionError> {
        Ok(self.now)
    }
    fn snapshot(
        &mut self,
        _: &RecoveryPolicy,
        _: Option<&ReservedAction>,
    ) -> Result<RecoverySnapshot, ExecutionError> {
        self.now += self.read_delay_ms;
        if self.fail_read {
            Err(ExecutionError::Transport("offline".into()))
        } else {
            Ok(self.o.clone())
        }
    }
    fn submit(&mut self, _: &RecoveryPolicy, a: &ReservedAction) -> SubmitOutcome {
        self.actions.push(a.clone());
        self.outcome
    }
}
#[test]
fn partial_close_reconciles_remaining_and_cleans_only_owned_exits_after_flat() {
    let (mut m, mut j, mut v) = fixture();
    m.tick(&mut j, &mut v).unwrap();
    assert!(
        matches!(&v.actions[0].action,RecoveryAction::CloseRemaining{side:Side::Buy,quantity,limit_price,..} if *quantity==d("1.1")&&*limit_price==d("96.48"))
    );
    v.at(3000);
    v.o.signed_position = d("-0.4");
    v.terminal();
    m.tick(&mut j, &mut v).unwrap();
    assert!(
        matches!(&v.actions[1].action,RecoveryAction::CloseRemaining{quantity,..} if *quantity==d("0.4"))
    );
    assert_ne!(v.actions[0].action.cloid(), v.actions[1].action.cloid());
    v.at(4000);
    v.o.signed_position = Decimal::ZERO;
    v.terminal();
    m.tick(&mut j, &mut v).unwrap();
    assert!(
        matches!(&v.actions[2].action,RecoveryAction::CancelOwned{cloid} if *cloid==m.policy().spec.tp_cloid)
    );
    v.at(5000);
    v.o.tp = None;
    m.tick(&mut j, &mut v).unwrap();
    assert!(
        matches!(&v.actions[3].action,RecoveryAction::CancelOwned{cloid} if *cloid==m.policy().spec.sl_cloid)
    );
    v.at(6000);
    v.o.sl = None;
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(m.state().phase, RecoveryPhase::Closed);
    assert!(m.entries_frozen());
    let alerts: Vec<_> = j
        .events
        .iter()
        .filter_map(|e| match &e.event {
            JournalEventKind::Recovery {
                notification: Some(n),
                ..
            } => Some(n.kind),
            _ => None,
        })
        .collect();
    assert!(alerts.contains(&hypercarry_notifications::Kind::StopResidual));
    assert!(alerts.contains(&hypercarry_notifications::Kind::PositionClosed));
}
#[test]
fn unknown_ack_and_restart_never_resubmit_until_original_is_terminal() {
    let (mut m, mut j, mut v) = fixture();
    v.outcome = SubmitOutcome::Unknown;
    m.tick(&mut j, &mut v).unwrap();
    let mut m = RecoveryManager::replay(&j.events).unwrap();
    v.at(3000);
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(v.actions.len(), 1);
    v.o.signed_position = d("-0.4");
    v.at(4000);
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(v.actions.len(), 1);
    v.terminal();
    v.at(5000);
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(v.actions.len(), 2);
}
#[test]
fn pending_entry_cancel_is_resolved_before_any_close() {
    let (mut m, mut j, mut v) = fixture();
    v.o.entry = EntryOutcome::Pending;
    m.tick(&mut j, &mut v).unwrap();
    assert!(
        matches!(&v.actions[0].action,RecoveryAction::CancelOwned{cloid} if *cloid==m.policy().spec.entry_cloid)
    );
    v.at(3000);
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(v.actions.len(), 1);
    v.at(4000);
    v.o.entry = EntryOutcome::Terminal;
    v.o.sl = None;
    v.o.tp = None;
    m.tick(&mut j, &mut v).unwrap();
    assert!(matches!(
        &v.actions[1].action,
        RecoveryAction::CloseRemaining { .. }
    ));
}
#[test]
fn native_stop_wins_the_race_and_no_fallback_close_is_sent() {
    let (mut m, mut j, mut v) = fixture();
    v.o.signed_position = Decimal::ZERO;
    v.o.tp = None;
    v.o.sl = None;
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(m.state().phase, RecoveryPhase::Closed);
    assert_eq!(v.actions, Vec::<ReservedAction>::new());
}
#[test]
fn acknowledgement_does_not_mean_closed_even_when_next_position_read_is_flat() {
    let (mut m, mut j, mut v) = fixture();
    m.tick(&mut j, &mut v).unwrap();
    v.at(3000);
    v.o.signed_position = Decimal::ZERO;
    v.o.tp = None;
    v.o.sl = None;
    m.tick(&mut j, &mut v).unwrap();
    assert_ne!(m.state().phase, RecoveryPhase::Closed);
    assert_eq!(v.actions.len(), 1);
    v.terminal();
    v.at(4000);
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(m.state().phase, RecoveryPhase::Closed);
}
#[test]
fn wrong_scope_stale_or_incomplete_reads_do_not_write_to_venue() {
    for mode in 0..4 {
        let (mut m, mut j, mut v) = fixture();
        match mode {
            0 => v.o.account = "other".into(),
            1 => v.o.complete = false,
            2 => v.o.started_at_ms = 1,
            _ => v.o.network = ExecutionNetwork::Mainnet,
        }
        m.tick(&mut j, &mut v).unwrap();
        assert_eq!(v.actions, Vec::<ReservedAction>::new());
        assert_eq!(m.state().phase, RecoveryPhase::Reconciling);
    }
}
#[test]
fn revoked_agent_unmanaged_exposure_and_gap_escalate_without_removing_stops() {
    for mode in 0..5 {
        let (mut m, mut j, mut v) = fixture();
        match mode {
            0 => v.o.agent_authorized = false,
            1 => v.o.unmanaged_orders = 1,
            2 => v.o.signed_position = d("-2"),
            3 => v.o.signed_position = d("1.1"),
            _ => v.o.ask = d("101"),
        }
        m.tick(&mut j, &mut v).unwrap();
        assert_eq!(v.actions, Vec::<ReservedAction>::new());
        assert_eq!(m.state().phase, RecoveryPhase::Escalated);
        v.at(3000);
        v.fail_read = true;
        m.tick(&mut j, &mut v).unwrap();
        v.at(4000);
        v.fail_read = false;
        v.o.agent_authorized = true;
        v.o.unmanaged_orders = 0;
        v.o.signed_position = d("-1.1");
        v.o.ask = d("96");
        m.tick(&mut j, &mut v).unwrap();
        assert_eq!(v.actions, Vec::<ReservedAction>::new());
        assert_eq!(m.state().phase, RecoveryPhase::Escalated);
    }
}
#[test]
fn attempt_cap_and_deadline_are_persistent() {
    let (mut m, mut j, mut v) = fixture();
    v.outcome = SubmitOutcome::Rejected;
    for now in [2000, 3000, 4000, 5000] {
        v.at(now);
        m.tick(&mut j, &mut v).unwrap();
    }
    assert_eq!(v.actions.len(), 3);
    assert_eq!(m.state().reason, Reason::AttemptsExhausted);
    let mut m = RecoveryManager::replay(&j.events).unwrap();
    v.at(6000);
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(v.actions.len(), 3);
    let (mut m, mut j, mut v) = fixture();
    v.outcome = SubmitOutcome::Unknown;
    m.tick(&mut j, &mut v).unwrap();
    v.at(32_001);
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(m.state().reason, Reason::Deadline);
    assert_eq!(v.actions.len(), 1);
}
#[test]
fn healthy_bracket_is_monitored_without_writes_or_resizing_native_exits() {
    let (mut m, mut j, mut v) = fixture();
    v.o.mark = d("82");
    v.o.stop_fired = false;
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(m.state().phase, RecoveryPhase::Monitoring);
    assert_eq!(v.actions, Vec::<ReservedAction>::new());
    v.at(3000);
    v.o.signed_position = d("-0.4");
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(v.actions, Vec::<ReservedAction>::new());
    v.at(4000);
    v.o.signed_position = d("-0.6");
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(m.state().reason, Reason::UnexpectedExposure);
    assert_eq!(v.actions, Vec::<ReservedAction>::new());
}
#[test]
fn missing_protection_flattens_but_does_not_cancel_the_surviving_exit() {
    let (mut m, mut j, mut v) = fixture();
    v.o.mark = d("82");
    v.o.stop_fired = false;
    v.o.sl = None;
    m.tick(&mut j, &mut v).unwrap();
    assert!(matches!(
        &v.actions[0].action,
        RecoveryAction::CloseRemaining { .. }
    ));
}
#[test]
#[allow(clippy::items_after_statements)]
fn gate_and_durable_write_failures_prevent_submission() {
    let (mut m, mut j, mut v) = fixture();
    v.fail_freeze = true;
    assert!(m.tick(&mut j, &mut v).is_err());
    assert_eq!(v.actions, Vec::<ReservedAction>::new());
    struct Broken(u64);
    impl Journal for Broken {
        fn next_sequence(&self) -> Option<u64> {
            Some(self.0)
        }
        fn append(
            &mut self,
            _: i64,
            _: &str,
            _: JournalEventKind,
        ) -> Result<JournalEvent, ExecutionError> {
            Err(ExecutionError::JournalSchema("disk full".into()))
        }
    }
    v.fail_freeze = false;
    let mut broken = Broken(j.events.len() as u64);
    assert!(m.tick(&mut broken, &mut v).is_err());
    assert_eq!(v.actions, Vec::<ReservedAction>::new());
}
#[test]
fn notifications_are_atomic_and_importable_without_a_trading_dependency_on_delivery() {
    use hypercarry_notifications::{Config, Outbox, Provider, Route, Severity, Source};
    use std::os::unix::fs::PermissionsExt;
    let (mut m, mut j, mut v) = fixture();
    m.tick(&mut j, &mut v).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("recovery.jsonl");
    let mut bytes = vec![];
    for event in &j.events {
        serde_json::to_writer(&mut bytes, event).unwrap();
        bytes.push(b'\n');
    }
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = Config {
        schema_version: 1,
        state_dir: dir.path().into(),
        sources: vec![Source {
            id: "recovery".into(),
            path,
        }],
        routes: vec![Route {
            id: "terminal".into(),
            minimum_severity: Severity::Critical,
            kinds: vec![],
            provider: Provider::Console,
        }],
        max_attempts: 2,
        retry_ms: 1000,
        poll_ms: 1000,
        delivery_timeout_ms: 60_000,
        sends_per_minute: 10,
        critical_reminder_ms: 60_000,
        max_reminders: 0,
    };
    let mut outbox = Outbox::open(config).unwrap();
    outbox.ingest(2000).unwrap();
    let n = outbox.state().deliveries.len();
    outbox.ingest(2000).unwrap();
    assert_eq!(n, outbox.state().deliveries.len());
    assert!(n >= 1);
}

#[test]
fn prolonged_read_outage_escalates_without_any_submission() {
    let (mut manager, mut journal, mut venue) = fixture();
    venue.fail_read = true;
    manager.tick(&mut journal, &mut venue).unwrap();
    venue.at(32_000);
    manager.tick(&mut journal, &mut venue).unwrap();
    assert_eq!(manager.state().phase, RecoveryPhase::Escalated);
    assert_eq!(manager.state().reason, Reason::Deadline);
    assert_eq!(venue.actions.len(), 0);
}
#[test]
fn definitely_rejected_entry_cancel_retries_boundedly_without_closing() {
    let (mut manager, mut journal, mut venue) = fixture();
    venue.o.entry = EntryOutcome::Pending;
    venue.outcome = SubmitOutcome::Rejected;
    for now in [2000, 3000, 4000, 5000, 6000] {
        venue.at(now);
        manager.tick(&mut journal, &mut venue).unwrap();
    }
    assert_eq!(venue.actions.len(), 4);
    assert!(
        venue
            .actions
            .iter()
            .all(|a| matches!(a.action, RecoveryAction::CancelOwned { .. }))
    );
    assert_eq!(manager.state().reason, Reason::AttemptsExhausted);
}
#[test]
fn replayed_grace_window_does_not_restart_and_dust_does_not_claim_flatness() {
    let (manager, _, mut venue) = fixture();
    let mut policy = manager.policy().clone();
    policy.limits.stop_grace_ms = 2000;
    // Replay a valid initial checkpoint with this separately reviewed fixture policy.
    let mut journal = InMemoryJournal::default();
    let state = manager.state().clone();
    journal
        .append(
            2000,
            &policy.spec.trade_id,
            JournalEventKind::Recovery {
                record: Box::new(RecoveryRecord {
                    policy: policy.clone(),
                    state,
                }),
                notification: None,
            },
        )
        .unwrap();
    let mut manager = RecoveryManager::replay(&journal.events).unwrap();
    manager.tick(&mut journal, &mut venue).unwrap();
    assert_eq!(manager.state().phase, RecoveryPhase::Grace);
    let mut manager = RecoveryManager::replay(&journal.events).unwrap();
    venue.at(3000);
    manager.tick(&mut journal, &mut venue).unwrap();
    assert_eq!(venue.actions.len(), 0);
    venue.at(4000);
    venue.o.signed_position = d("-0.001");
    manager.tick(&mut journal, &mut venue).unwrap();
    assert_eq!(manager.state().reason, Reason::Dust);
    assert_eq!(venue.actions.len(), 0);
}
#[test]
fn long_position_uses_sell_to_close_with_inward_rounded_bound() {
    let (manager, _, mut venue) = fixture();
    let mut policy = manager.policy().clone();
    policy.spec.entry_side = Side::Buy;
    policy.spec.tp = d("95");
    policy.spec.sl = d("70");
    let mut journal = InMemoryJournal::default();
    journal
        .append(
            2000,
            &policy.spec.trade_id,
            JournalEventKind::Recovery {
                record: Box::new(RecoveryRecord {
                    policy: policy.clone(),
                    state: manager.state().clone(),
                }),
                notification: None,
            },
        )
        .unwrap();
    let mut manager = RecoveryManager::replay(&journal.events).unwrap();
    venue.o.signed_position = d("1.1");
    venue.o.mark = d("69");
    venue.o.bid = d("69");
    venue.o.ask = d("69.01");
    venue.o.tp = None;
    venue.o.sl = None;
    manager.tick(&mut journal, &mut venue).unwrap();
    assert!(
        matches!(&venue.actions[0].action,RecoveryAction::CloseRemaining{side:Side::Sell,quantity,limit_price,..}if *quantity==d("1.1")&&*limit_price==d("68.66"))
    );
}

#[test]
fn durable_recovery_journal_round_trips_unknown_exposure_and_reserved_actions() {
    let (mut manager, mut memory, mut venue) = fixture();
    manager.tick(&mut memory, &mut venue).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery.jsonl");
    let mut file = hypercarry_execution::FileJournal::open(&path).unwrap();
    for event in &memory.events {
        file.append(
            event.recorded_at_ms,
            &event.correlation_id,
            event.event.clone(),
        )
        .unwrap();
    }
    drop(file);
    let records = hypercarry_execution::read_journal(&path).unwrap();
    let resumed = RecoveryManager::replay(&records).unwrap();
    assert_eq!(resumed.state(), manager.state());
    assert_eq!(resumed.policy(), manager.policy());
}

#[test]
fn coarse_tick_cannot_consume_attempts_with_a_nonmarketable_close() {
    let (_, j, mut v) = fixture();
    let mut records = j.events;
    for r in &mut records {
        if let JournalEventKind::Recovery { record, .. } = &mut r.event {
            record.policy.limits.price_tick = d("1");
            record.policy.limits.slippage_bps = 1;
        }
    }
    let mut m = RecoveryManager::replay(&records).unwrap();
    let mut j = InMemoryJournal::default();
    for record in records {
        j.append(record.recorded_at_ms, &record.correlation_id, record.event)
            .unwrap();
    }
    v.o.ask = d("96.5");
    v.o.bid = d("96.49");
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(v.actions.len(), 0);
    assert_eq!(m.state().phase, RecoveryPhase::Escalated);
    assert_eq!(m.state().reason, Reason::PriceOutsideBound);
    assert_eq!(m.state().close_attempts, 0);
    let mut invalid_spec = m.policy().spec.clone();
    invalid_spec.coin = "A".repeat(33);
    assert!(invalid_spec.validate().is_err());
}

#[test]
fn a_failed_read_crossing_the_incident_deadline_escalates_in_the_same_pass() {
    let (mut m, mut j, mut v) = fixture();
    v.fail_read = true;
    m.tick(&mut j, &mut v).unwrap();
    v.at(2100);
    v.read_delay_ms = 30_000;
    m.tick(&mut j, &mut v).unwrap();
    assert_eq!(m.state().phase, RecoveryPhase::Escalated);
    assert_eq!(m.state().reason, Reason::Deadline);
    assert_eq!(m.state().last_time_ms, 32_100);
    assert_eq!(v.actions.len(), 0);
}

#[test]
fn repaired_exit_handoff_preserves_spec_and_survives_recovery_replay() {
    let (original, _, mut venue) = fixture();
    let spec = original.policy().spec.clone();
    let mut bracket_journal = InMemoryJournal::default();
    let mut bracket = BracketManager::create(spec.clone(), &mut bracket_journal, 1000).unwrap();
    bracket
        .record(
            &mut bracket_journal,
            1001,
            BracketEvent::PlanApproved {
                spec_digest: spec.digest().unwrap(),
                expires_at_ms: 10_000,
            },
        )
        .unwrap();
    bracket
        .record(
            &mut bracket_journal,
            1002,
            BracketEvent::ConfigurationVerified {
                isolated: true,
                leverage: 1,
            },
        )
        .unwrap();
    bracket
        .record(
            &mut bracket_journal,
            1003,
            BracketEvent::EntryReserved { nonce: 1003 },
        )
        .unwrap();
    bracket
        .record(
            &mut bracket_journal,
            1005,
            BracketEvent::Observed {
                observation: BracketObservation {
                    account: spec.account.clone(),
                    network: spec.network,
                    coin: spec.coin.clone(),
                    entry_outcome: EntryOutcome::Terminal,
                    signed_position: d("-0.7"),
                    tp: None,
                    sl: None,
                    unmanaged_orders: 0,
                    started_at_ms: 1004,
                    completed_at_ms: 1005,
                    complete: true,
                },
            },
        )
        .unwrap();
    let tp = format!("0x{}", "55".repeat(16));
    let sl = format!("0x{}", "66".repeat(16));
    bracket
        .record(
            &mut bracket_journal,
            1006,
            BracketEvent::ProtectionRepairReserved {
                take_profit_cloid: Some(tp.clone()),
                stop_loss_cloid: Some(sl.clone()),
            },
        )
        .unwrap();
    let bracket = BracketManager::replay(&bracket_journal.events).unwrap();
    let mut journal = InMemoryJournal::default();
    let mut manager = RecoveryManager::from_bracket(
        &bracket,
        original.policy().limits.clone(),
        &mut journal,
        2000,
    )
    .unwrap();
    assert_eq!(
        manager.policy().spec.digest().unwrap(),
        spec.digest().unwrap()
    );
    venue.o.signed_position = d("-0.7");
    venue.o.tp.as_mut().unwrap().cloid = tp.clone();
    venue.o.sl.as_mut().unwrap().cloid = sl.clone();
    manager.tick(&mut journal, &mut venue).unwrap();
    assert!(matches!(
        venue.actions[0].action,
        RecoveryAction::CloseRemaining { .. }
    ));
    let mut manager = RecoveryManager::replay(&journal.events).unwrap();
    assert_eq!(manager.policy().tp_cloid(), tp);
    assert_eq!(manager.policy().sl_cloid(), sl);
    venue.at(3000);
    venue.terminal();
    venue.o.signed_position = Decimal::ZERO;
    manager.tick(&mut journal, &mut venue).unwrap();
    assert_eq!(
        venue.actions.last().unwrap().action,
        RecoveryAction::CancelOwned { cloid: tp }
    );
}
