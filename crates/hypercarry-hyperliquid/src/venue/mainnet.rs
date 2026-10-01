//! Mainnet facade: no public raw exchange transport or ungated placement API.
use super::{
    ClientOrderId, Clock, Decimal, Duration, ExecutionError, ExecutionMode, ExecutionNetwork,
    HyperliquidAssetResolver, HyperliquidConnection, HyperliquidL1Signer,
    HyperliquidSigningRequest, HyperliquidTestnetConfig, HyperliquidTestnetExecutor,
    HyperliquidTransport, HyperliquidWebSocketTransport, Journal, JournalEvent, JournalEventKind,
    LiveOrder, MarketConnection, MarketTransport, Network, PrivateEvent, PrivateEventOutcome,
    PrivateStream, RequestThrottle, ReqwestHyperliquidTransport, RetryPolicy, RiskPolicy, Side,
    TestnetAcknowledgement, ValidatedOrder, Value, json, private_subscription_requests,
    transport_error, validate_address, validate_provider_alias, venue_schema,
};
use hypercarry_execution::{
    DeadManSwitch, ExplicitMainnetEnable, InteractiveMainnetConfirmation, MainnetAuthorization,
    MainnetReleaseGate, OperationalHealthSource, Signer,
};

/// Pinned mainnet trading-account and external signer identity.
#[derive(Debug, Clone)]
pub struct HyperliquidMainnetConfig {
    account_address: String,
    signer_address: String,
    action_ttl_ms: u64,
}

impl HyperliquidMainnetConfig {
    /// Validates canonical account/agent addresses and bounded action expiry.
    ///
    /// # Errors
    /// Rejects invalid addresses, master-key signing, or TTL outside 1..=60000ms.
    pub fn new(
        account_address: String,
        signer_address: String,
        action_ttl_ms: u64,
    ) -> Result<Self, ExecutionError> {
        validate_address(&account_address)?;
        validate_address(&signer_address)?;
        if account_address == signer_address || action_ttl_ms == 0 || action_ttl_ms > 60_000 {
            return Err(ExecutionError::Validation(
                "mainnet requires a separate agent and action TTL in 1..=60000ms".into(),
            ));
        }
        Ok(Self {
            account_address,
            signer_address,
            action_ttl_ms,
        })
    }
}

/// Mainnet adapter with a private HTTPS transport. Each placement consumes one
/// exact-order capability; cancellations and reconciliation remain available
/// when release health or the emergency stop blocks new placements.
pub struct HyperliquidMainnetExecutor<S, J, C, A, H, D> {
    inner: HyperliquidTestnetExecutor<S, ReqwestHyperliquidTransport, J, C, A>,
    gate: MainnetReleaseGate<H, D>,
}

impl<S, J, C, A, H, D> HyperliquidMainnetExecutor<S, J, C, A, H, D>
where
    S: HyperliquidL1Signer,
    J: Journal,
    C: Clock,
    A: HyperliquidAssetResolver,
    H: OperationalHealthSource,
    D: DeadManSwitch,
{
    /// Builds a mainnet adapter without submitting or signing anything.
    /// The host must bind this configuration and binary to its reviewed bundle.
    ///
    /// # Errors
    /// Rejects signer identity/network mismatch or invalid transport bounds.
    pub fn new(
        config: HyperliquidMainnetConfig,
        signer: S,
        journal: J,
        clock: C,
        assets: A,
        gate: MainnetReleaseGate<H, D>,
    ) -> Result<Self, ExecutionError> {
        validate_provider_alias(signer.key_id())?;
        if signer.network() != ExecutionNetwork::Mainnet
            || signer.signer_address() != config.signer_address
        {
            return Err(ExecutionError::Validation(
                "mainnet signer identity mismatch".into(),
            ));
        }
        let mut transport =
            ReqwestHyperliquidTransport::new(Duration::from_secs(5), Duration::from_secs(10))?;
        transport.endpoint = "https://api.hyperliquid.xyz";
        Ok(Self {
            inner: HyperliquidTestnetExecutor {
                config: HyperliquidTestnetConfig {
                    account_address: config.account_address,
                    authorized_signer_address: config.signer_address,
                    action_ttl_ms: config.action_ttl_ms,
                    network: ExecutionNetwork::Mainnet,
                    _acknowledgement: TestnetAcknowledgement(()),
                },
                signer,
                transport,
                journal,
                clock,
                assets,
                throttle: RequestThrottle::new(20, 1_000)?,
                retry: RetryPolicy::new(2, 100, 1_000)?,
                last_nonce: None,
            },
            gate,
        })
    }

    /// Authorizes an exact order using explicit manual inputs, without signing.
    ///
    /// # Errors
    /// Fails on incomplete evidence, canary limits, or unhealthy runtime state.
    pub fn authorize(
        &self,
        order: &ValidatedOrder,
        enable: ExplicitMainnetEnable,
        confirmation: InteractiveMainnetConfirmation,
    ) -> Result<MainnetAuthorization, ExecutionError> {
        self.gate.authorize_with_clock(
            order,
            || self.inner.clock.now_ms(),
            enable,
            confirmation,
            &Identity(&self.inner.signer),
        )
    }

    /// Consumes the capability immediately and submits only its embedded order.
    /// No second order argument, automatic retry, or public raw transport exists.
    ///
    /// # Errors
    /// Fails closed on expiry, bundle mismatch, risk, signing or transport error.
    pub fn place<P: RiskPolicy>(
        &mut self,
        authorization: MainnetAuthorization,
        policy: &P,
    ) -> Result<LiveOrder, ExecutionError> {
        place_authorized(&mut self.inner, &self.gate, authorization, policy)
    }

    /// Cancels a managed mainnet order without requiring permission to add risk.
    ///
    /// # Errors
    /// Returns a signing, lifecycle or transport failure; retain the journal.
    pub fn cancel(&mut self, live: &mut LiveOrder) -> Result<(), ExecutionError> {
        ensure_mainnet(live)?;
        self.inner.cancel(live)
    }

    /// Arms or disarms the venue-side dead man's switch for this account.
    ///
    /// Deliberately not gated behind a [`MainnetAuthorization`]. That capability
    /// exists to make *adding* risk a single, reviewed, non-repeatable act; this
    /// only removes risk, and requiring permission to protect the account would
    /// make the protection unavailable in exactly the conditions — unhealthy
    /// runtime, engaged kill switch, expired evidence — that call for it. It sits
    /// alongside [`cancel`](Self::cancel) for the same reason.
    ///
    /// The switch cancels open orders and does **not** close positions; see
    /// the underlying method for what that means for recovery.
    ///
    /// # Errors
    /// Fails on a deadline under five seconds away, signing, journal, venue
    /// rejection, or a transport outcome leaving the armed state unknown.
    pub fn schedule_cancel(&mut self, at_ms: Option<i64>) -> Result<(), ExecutionError> {
        self.inner.schedule_cancel(at_ms)
    }

    /// Reconciles by the durable client order ID without placing an order.
    ///
    /// # Errors
    /// Fails on transport errors or ambiguous venue state.
    pub fn reconcile(&mut self, live: &mut LiveOrder) -> Result<(), ExecutionError> {
        ensure_mainnet(live)?;
        self.inner.reconcile(live)
    }

    /// Replays a mainnet action and its durable lifecycle for recovery only.
    ///
    /// # Errors
    /// Rejects journals without the exact mainnet action or inconsistent events.
    pub fn recover(
        order: ValidatedOrder,
        events: &[JournalEvent],
    ) -> Result<LiveOrder, ExecutionError> {
        if !events.iter().any(|e| matches!(&e.event, JournalEventKind::ExactAction { mode: ExecutionMode::Mainnet, order: recorded } if recorded == &order)) {
            return Err(ExecutionError::Lifecycle("recovery requires an exact mainnet journal action".into()));
        }
        let id = ClientOrderId::derive(&order)?;
        let mut live = LiveOrder::recover(order, id, events)?;
        live.mode = ExecutionMode::Mainnet;
        Ok(live)
    }

    /// Applies a private-stream observation to a managed mainnet order.
    ///
    /// # Errors
    /// Rejects wrong identities, duplicate inconsistencies or impossible states.
    pub fn apply_private_event(
        &mut self,
        live: &mut LiveOrder,
        event: &PrivateEvent,
    ) -> Result<PrivateEventOutcome, ExecutionError> {
        ensure_mainnet(live)?;
        self.inner.apply_private_event(live, event)
    }

    /// Queries default-perp-DEX and spot open orders for final-state checking.
    ///
    /// # Errors
    /// Fails for transport or malformed responses.
    pub fn open_orders(&mut self) -> Result<Vec<Value>, ExecutionError> {
        self.inner.admit_request()?;
        let response = self
            .inner
            .transport
            .info(&json!({"type":"openOrders","user":self.inner.config.account_address}))
            .map_err(|e| transport_error(&e))?;
        let orders = response
            .as_array()
            .ok_or_else(|| venue_schema("open orders must be an array"))?;
        if orders.iter().any(|o| !o.is_object()) {
            return Err(venue_schema("open order must be an object"));
        }
        Ok(orders.clone())
    }
}

fn ensure_mainnet(live: &LiveOrder) -> Result<(), ExecutionError> {
    if live.mode != ExecutionMode::Mainnet {
        return Err(ExecutionError::Validation("order is not mainnet".into()));
    }
    Ok(())
}

// This bridge exposes identity to the venue-neutral gate, never a second signing path.
struct Identity<'a, S>(&'a S);
impl<S: HyperliquidL1Signer> Signer for Identity<'_, S> {
    type Error = std::io::Error;
    fn network(&self) -> ExecutionNetwork {
        self.0.network()
    }
    fn key_id(&self) -> &str {
        self.0.key_id()
    }
    fn sign(&self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Err(std::io::Error::other("opaque signing is disabled"))
    }
}

/// Connects a dedicated mainnet private stream using unsigned subscriptions.
///
/// # Errors
/// Fails on invalid account identity, connection or subscription writes.
pub async fn connect_mainnet_private_stream(
    account: &str,
) -> Result<PrivateStream<HyperliquidConnection>, ExecutionError> {
    let subscriptions = private_subscription_requests(account)?;
    let transport = HyperliquidWebSocketTransport::for_network(Network::Mainnet);
    let mut connection = transport.connect().await.map_err(|_| {
        ExecutionError::Transport("mainnet private stream connection failed".into())
    })?;
    for subscription in subscriptions {
        connection
            .send_text(serde_json::to_string(&subscription)?)
            .await
            .map_err(|_| ExecutionError::Transport("mainnet private subscription failed".into()))?;
    }
    Ok(PrivateStream { connection })
}

pub(super) fn verify_mainnet_signature(
    signed: &Value,
    request: &HyperliquidSigningRequest,
    address: &str,
) -> Result<(), ExecutionError> {
    let action: hypersdk::hypercore::Action = serde_json::from_value(request.action.clone())
        .map_err(|_| venue_schema("invalid mainnet signing action"))?;
    let signature: hypersdk::hypercore::Signature =
        serde_json::from_value(signed["signature"].clone())
            .map_err(|_| venue_schema("invalid mainnet signature encoding"))?;
    let expiry = i64::try_from(request.expires_after)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .ok_or_else(|| venue_schema("mainnet signature expiry is invalid"))?;
    let recovered = action
        .recover(
            &signature,
            request.nonce,
            None,
            Some(expiry),
            hypersdk::hypercore::Chain::Mainnet,
        )
        .map_err(|_| venue_schema("mainnet signature recovery failed"))?;
    if recovered.to_string().to_ascii_lowercase() != address {
        return Err(venue_schema(
            "signature does not recover the pinned mainnet agent",
        ));
    }
    Ok(())
}

fn place_authorized<S, T, J, C, A, H, D, P>(
    inner: &mut HyperliquidTestnetExecutor<S, T, J, C, A>,
    gate: &MainnetReleaseGate<H, D>,
    authorization: MainnetAuthorization,
    policy: &P,
) -> Result<LiveOrder, ExecutionError>
where
    S: HyperliquidL1Signer,
    T: HyperliquidTransport,
    J: Journal,
    C: Clock,
    A: HyperliquidAssetResolver,
    H: OperationalHealthSource,
    D: DeadManSwitch,
    P: RiskPolicy,
{
    let now = inner.clock.now_ms()?;
    let issued = authorization.authorized_at_ms();
    let expires = authorization.expires_at_ms();
    let digest = authorization.evidence_digest().to_owned();
    let order = authorization.into_order(now)?;
    inner.place_validated(&order, policy, Some((issued, expires)), |signer, clock| {
        gate.recheck_submission_with_clock(&order, &digest, || clock.now_ms(), &Identity(signer))?;
        let after_checks = clock.now_ms()?;
        if after_checks < issued || after_checks > expires {
            return Err(ExecutionError::Policy(
                "authorization expired during continuous checks".into(),
            ));
        }
        Ok(())
    })
}

pub(super) fn validate_status_identity(
    response: &Value,
    live: &LiveOrder,
) -> Result<(), ExecutionError> {
    if response.as_str() == Some("unknownOid") || response["status"] == "unknownOid" {
        return Ok(());
    }
    let wire = response
        .pointer("/order/order")
        .ok_or_else(|| venue_schema("missing mainnet order status identity"))?;
    let expected = live.order();
    let cloid = live.client_order_id().to_string();
    if wire["coin"] != expected.market
        || wire["cloid"] != cloid
        || wire["side"].as_str() != Some(if expected.side == Side::Buy { "B" } else { "A" })
        || wire["origSz"]
            .as_str()
            .and_then(|s| s.parse::<Decimal>().ok())
            != Some(expected.quantity)
        || wire["limitPx"]
            .as_str()
            .and_then(|s| s.parse::<Decimal>().ok())
            != Some(expected.limit_price)
    {
        return Err(venue_schema(
            "mainnet order status differs from exact managed order",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::venue::{OrderState, RiskDecision, TransportFailure};
    use hypercarry_execution::{
        InMemoryJournal, MainnetReleaseConfig, MainnetReleaseEvidence, OperationalHealth, Readiness,
    };
    use std::{cell::Cell, rc::Rc};

    #[derive(Clone)]
    struct TestClock(Rc<Cell<i64>>);
    impl Clock for TestClock {
        fn now_ms(&self) -> Result<i64, ExecutionError> {
            Ok(self.0.get())
        }
    }
    struct Health(Rc<Cell<bool>>);
    impl OperationalHealthSource for Health {
        fn health(&self) -> Result<OperationalHealth, ExecutionError> {
            Ok(OperationalHealth {
                observed_at_ms: 10_000,
                startup_reconciliation: Readiness::Ready,
                continuous_reconciliation: Readiness::Ready,
                unmanaged_orders: 0,
                unresolved_submissions: 0,
                rest_latency_ms: 1,
                private_latency_ms: 1,
                private_stream: if self.0.get() {
                    Readiness::Ready
                } else {
                    Readiness::NotReady
                },
                alerts: Readiness::Ready,
                audit_journal: Readiness::Ready,
                rollback: Readiness::Ready,
            })
        }
    }
    struct DeadMan;
    impl DeadManSwitch for DeadMan {
        fn is_armed(&self, _: i64) -> Result<bool, ExecutionError> {
            Ok(true)
        }
    }
    struct Assets;
    impl HyperliquidAssetResolver for Assets {
        fn asset_id(&self, _: &ValidatedOrder) -> Result<u32, ExecutionError> {
            Ok(0)
        }
    }
    struct Allow;
    impl RiskPolicy for Allow {
        fn evaluate(&self, _: &ValidatedOrder) -> Result<RiskDecision, ExecutionError> {
            Ok(RiskDecision::Allow {
                code: "test".into(),
                reason: "fixture".into(),
            })
        }
    }
    struct TestSigner {
        clock: TestClock,
        healthy: Rc<Cell<bool>>,
        delay: i64,
        break_health: bool,
        wrong_domain: bool,
        calls: Rc<Cell<u32>>,
        address: String,
    }
    impl HyperliquidL1Signer for TestSigner {
        type Error = std::io::Error;
        fn network(&self) -> ExecutionNetwork {
            ExecutionNetwork::Mainnet
        }
        fn key_id(&self) -> &'static str {
            "provider/mainnet"
        }
        fn signer_address(&self) -> &str {
            &self.address
        }
        fn sign_l1_action(
            &self,
            request: &HyperliquidSigningRequest,
        ) -> Result<Value, Self::Error> {
            assert_eq!(request.network(), ExecutionNetwork::Mainnet);
            self.calls.set(self.calls.get() + 1);
            self.clock.0.set(self.clock.0.get() + self.delay);
            if self.break_health {
                self.healthy.set(false);
            }
            let action: hypersdk::hypercore::Action =
                serde_json::from_value(request.action.clone()).unwrap();
            let signed = action
                .sign_sync(
                    &local_signer(),
                    request.nonce,
                    None,
                    chrono::DateTime::from_timestamp_millis(
                        i64::try_from(request.expires_after).unwrap(),
                    ),
                    if self.wrong_domain {
                        hypersdk::hypercore::Chain::Testnet
                    } else {
                        hypersdk::hypercore::Chain::Mainnet
                    },
                )
                .unwrap();
            let mut value = serde_json::to_value(&signed).unwrap();
            value["signature"]["r"] = format!("0x{:064x}", signed.signature.r).into();
            value["signature"]["s"] = format!("0x{:064x}", signed.signature.s).into();
            Ok(value)
        }
    }
    fn local_signer() -> hypersdk::hypercore::PrivateKeySigner {
        // Public deterministic SDK test key; never funded or used for live requests.
        "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e"
            .parse()
            .unwrap()
    }
    #[derive(Default)]
    struct Transport {
        requests: Vec<Value>,
        malformed: bool,
        info_response: Option<Value>,
    }
    impl HyperliquidTransport for Transport {
        fn exchange(&mut self, request: &Value) -> Result<Value, TransportFailure> {
            self.requests.push(request.clone());
            Ok(if self.malformed {
                json!({"unexpected":true})
            } else if request["action"]["type"] == "cancelByCloid" {
                json!({"status":"ok","response":{"type":"cancel","data":{"statuses":["success"]}}})
            } else {
                json!({"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":42}}]}}})
            })
        }
        fn info(&mut self, _: &Value) -> Result<Value, TransportFailure> {
            Ok(self
                .info_response
                .clone()
                .unwrap_or_else(|| json!({"status":"unknownOid"})))
        }
    }
    type Engine =
        HyperliquidTestnetExecutor<TestSigner, Transport, InMemoryJournal, TestClock, Assets>;
    fn fixture(
        delay: i64,
        break_health: bool,
        wrong_domain: bool,
    ) -> (Engine, MainnetReleaseGate<Health, DeadMan>, ValidatedOrder) {
        let clock = TestClock(Rc::new(Cell::new(10_000)));
        let healthy = Rc::new(Cell::new(true));
        let signer = TestSigner {
            clock: clock.clone(),
            healthy: healthy.clone(),
            delay,
            break_health,
            wrong_domain,
            calls: Rc::new(Cell::new(0)),
            address: local_signer().address().to_string().to_ascii_lowercase(),
        };
        let engine = HyperliquidTestnetExecutor {
            config: HyperliquidTestnetConfig {
                account_address: "0x1111111111111111111111111111111111111111".into(),
                authorized_signer_address: signer.address.clone(),
                action_ttl_ms: 10_000,
                network: ExecutionNetwork::Mainnet,
                _acknowledgement: TestnetAcknowledgement(()),
            },
            signer,
            transport: Transport::default(),
            journal: InMemoryJournal::default(),
            clock,
            assets: Assets,
            throttle: RequestThrottle::new(20, 1000).unwrap(),
            retry: RetryPolicy::new(2, 100, 1000).unwrap(),
            last_nonce: None,
        };
        let config: MainnetReleaseConfig = serde_json::from_value(json!({
            "source_commit":"a".repeat(40),"cargo_lock_digest":"11".repeat(32),"allowed_market":"hyperliquid:BTC",
            "canary_notional_limit":"100","reviewed_max_canary_notional":"100","testnet_key_id":"provider/testnet",
            "config_revision":"canary-v1","config_review":"review/config","max_health_age_ms":20000,
            "max_rest_latency_ms":500,"max_private_latency_ms":500,"authorization_ttl_ms":5000
        })).unwrap();
        let sessions: Vec<_> = (0..3).map(|i| json!({"session_id":format!("fixture-{i}"),"commit":"aaaaaaa", "started_at_ms":i*1000,"ended_at_ms":i*1000+900,
            "independent_reviewer":"reviewer","final_state_check":"check/final","orphaned_orders":0,"unmanaged_orders":0,"unresolved_submissions":0,"risk_limit_bypasses":0,"secret_leaks":0})).collect();
        let mut evidence: MainnetReleaseEvidence = serde_json::from_value(json!({"schema_version":2,"testnet_sessions":sessions,
            "reviewed_bundle":{"source_commit":config.source_commit,"cargo_lock_digest":config.cargo_lock_digest,"mainnet_transport_artifact":"fixture/transport",
                "mainnet_signer_key_id":"provider/mainnet","config_digest":config.review_digest().unwrap(),"dependency_audit":"fixture/audit","execution_security_review":"fixture/security","rollback_test":"fixture/rollback"},
            "release_decision":{"approved":true,"approver":"fixture/operator","decided_at_ms":5000,"evidence_digest":""}})).unwrap();
        evidence.release_decision.evidence_digest = evidence.evidence_digest().unwrap();
        let gate = MainnetReleaseGate::new(config, evidence, Health(healthy), DeadMan).unwrap();
        let order = ValidatedOrder {
            schema_version: 1,
            correlation_id: "fixture-mainnet".into(),
            venue: "hyperliquid".into(),
            market: "BTC".into(),
            side: Side::Buy,
            quantity: Decimal::ONE,
            limit_price: Decimal::from(100),
            created_at_ms: 10_000,
        };
        (engine, gate, order)
    }
    fn authorization(
        engine: &Engine,
        gate: &MainnetReleaseGate<Health, DeadMan>,
        order: &ValidatedOrder,
    ) -> MainnetAuthorization {
        gate.authorize(
            order,
            10_000,
            ExplicitMainnetEnable::from_cli_flag(true).unwrap(),
            InteractiveMainnetConfirmation::new(hypercarry_execution::MAINNET_CONFIRMATION)
                .unwrap(),
            &Identity(&engine.signer),
        )
        .unwrap()
    }
    #[test]
    fn mainnet_consumes_exact_order_and_records_correct_network() {
        let (mut engine, gate, order) = fixture(0, false, false);
        let token = authorization(&engine, &gate, &order);
        let live = place_authorized(&mut engine, &gate, token, &Allow).unwrap();
        assert_eq!(live.order(), &order);
        assert_eq!(live.report().mode, ExecutionMode::Mainnet);
        assert_eq!(engine.transport.requests.len(), 1);
        assert_eq!(
            engine.transport.requests[0]["action"]["orders"][0]["p"],
            "100"
        );
        assert!(engine.journal.events.iter().any(|e| matches!(
            e.event,
            JournalEventKind::ExactAction {
                mode: ExecutionMode::Mainnet,
                ..
            }
        )));
    }
    #[test]
    fn expired_or_unhealthy_during_signing_never_submits() {
        for (delay, health, domain) in [(5001, false, false), (0, true, false), (0, false, true)] {
            let (mut engine, gate, order) = fixture(delay, health, domain);
            let token = authorization(&engine, &gate, &order);
            assert!(place_authorized(&mut engine, &gate, token, &Allow).is_err());
            assert_eq!(engine.signer.calls.get(), 1);
            assert!(engine.transport.requests.is_empty());
        }
    }
    #[test]
    fn expired_before_consumption_never_signs() {
        let (mut engine, gate, order) = fixture(0, false, false);
        let token = authorization(&engine, &gate, &order);
        engine.clock.0.set(15_001);
        assert!(place_authorized(&mut engine, &gate, token, &Allow).is_err());
        assert_eq!(engine.signer.calls.get(), 0);
    }
    #[test]
    fn malformed_exchange_acknowledgement_is_durably_uncertain() {
        let (mut engine, gate, order) = fixture(0, false, false);
        engine.transport.malformed = true;
        let token = authorization(&engine, &gate, &order);
        assert!(place_authorized(&mut engine, &gate, token, &Allow).is_err());
        let recovered = LiveOrder::recover(
            order.clone(),
            ClientOrderId::derive(&order).unwrap(),
            &engine.journal.events,
        )
        .unwrap();
        assert_eq!(recovered.state(), OrderState::SubmissionUncertain);
        assert_eq!(engine.transport.requests.len(), 1);
    }
    #[test]
    fn mainnet_cancel_reconcile_and_restart_preserve_identity() {
        type Adapter = HyperliquidMainnetExecutor<
            TestSigner,
            InMemoryJournal,
            TestClock,
            Assets,
            Health,
            DeadMan,
        >;
        let (mut engine, gate, order) = fixture(0, false, false);
        let token = authorization(&engine, &gate, &order);
        let mut live = place_authorized(&mut engine, &gate, token, &Allow).unwrap();
        // Emergency cancellation works after health blocks new placements.
        engine.signer.healthy.set(false);
        engine.cancel(&mut live).unwrap();
        assert_eq!(
            engine.transport.requests[1]["action"]["type"],
            "cancelByCloid"
        );
        let response = json!({"status":"order","order":{"status":"canceled","statusTimestamp":10002,
            "order":{"coin":"BTC","cloid":live.client_order_id(),"side":"B","origSz":"1","sz":"1","limitPx":"100","oid":42}}});
        let mut wrong = response.clone();
        wrong["order"]["order"]["coin"] = "ETH".into();
        engine.transport.info_response = Some(wrong);
        assert!(engine.reconcile(&mut live).is_err());
        assert_eq!(live.state(), OrderState::CancelPending);
        engine.transport.info_response = Some(response);
        engine.reconcile(&mut live).unwrap();
        assert_eq!(live.state(), OrderState::Cancelled);

        let recovered = Adapter::recover(order, &engine.journal.events).unwrap();
        assert_eq!(recovered.report().mode, ExecutionMode::Mainnet);
        assert_eq!(recovered.state(), OrderState::Cancelled);
        assert_eq!(recovered.client_order_id(), live.client_order_id());
    }

    #[test]
    fn deadline_expiring_during_durable_append_prevents_submission() {
        struct DelayedJournal {
            clock: TestClock,
            journal: InMemoryJournal,
        }
        impl Journal for DelayedJournal {
            fn append(
                &mut self,
                time: i64,
                id: &str,
                event: JournalEventKind,
            ) -> Result<JournalEvent, ExecutionError> {
                if matches!(&event, JournalEventKind::LifecycleTransition {transition} if transition.to == OrderState::SubmissionPending)
                {
                    self.clock.0.set(15_001);
                }
                self.journal.append(time, id, event)
            }
        }
        let (engine, gate, order) = fixture(0, false, false);
        let token = authorization(&engine, &gate, &order);
        let mut delayed = HyperliquidTestnetExecutor {
            config: engine.config,
            signer: engine.signer,
            transport: engine.transport,
            journal: DelayedJournal {
                clock: engine.clock.clone(),
                journal: engine.journal,
            },
            clock: engine.clock,
            assets: engine.assets,
            throttle: engine.throttle,
            retry: engine.retry,
            last_nonce: engine.last_nonce,
        };
        assert!(place_authorized(&mut delayed, &gate, token, &Allow).is_err());
        assert!(delayed.transport.requests.is_empty());
    }
}
