use crate::{
    ClientOrderId, ExecutionError, ExecutionMode, ExecutionNetwork, ExecutionReport, Journal,
    JournalEvent, JournalEventKind, LifecycleTransition, MarketMetadataResolver, OrderIntent,
    OrderState, OrderStateMachine, RecoveryAction, Rejection, RequestThrottle, RetryPolicy,
    RiskDecision, RiskPolicy, Side, SubmissionFailure, ThrottleDecision, TransitionOutcome,
    ValidatedOrder,
};
use hypercarry_core::info::Network;
use hypercarry_recorder::{
    HyperliquidWebSocketTransport, MarketConnection, MarketTransport,
    transport::{HyperliquidConnection, TransportMessage},
};
use reqwest::{StatusCode, blocking::Client, header::RETRY_AFTER};
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const TESTNET_API: &str = "https://api.hyperliquid-testnet.xyz";
const MAX_RESPONSE_BYTES: usize = 1_048_576;

/// Exact operator acknowledgement required to construct a testnet executor.
pub const TESTNET_ACKNOWLEDGEMENT: &str = "I ACKNOWLEDGE HYPERCARRY TESTNET EXECUTION";

/// Validated, unmistakable acknowledgement of testnet external effects.
#[derive(Debug, Clone, Copy)]
pub struct TestnetAcknowledgement(());

impl TestnetAcknowledgement {
    /// Validates the exact acknowledgement phrase.
    ///
    /// # Errors
    ///
    /// Returns an error for any other text.
    pub fn new(value: &str) -> Result<Self, ExecutionError> {
        if value != TESTNET_ACKNOWLEDGEMENT {
            return Err(ExecutionError::Policy(format!(
                "testnet execution requires exact acknowledgement `{TESTNET_ACKNOWLEDGEMENT}`"
            )));
        }
        Ok(Self(()))
    }
}

/// Testnet account and request-expiry configuration.
#[derive(Debug, Clone)]
pub struct HyperliquidTestnetConfig {
    account_address: String,
    authorized_signer_address: String,
    action_ttl_ms: u64,
    _acknowledgement: TestnetAcknowledgement,
}

impl HyperliquidTestnetConfig {
    /// Creates testnet-only configuration.
    ///
    /// # Errors
    ///
    /// Returns an error unless the account is a lowercase 20-byte hex address
    /// and the action TTL is in `(0, 60_000]` milliseconds.
    pub fn new(
        account_address: impl Into<String>,
        action_ttl_ms: u64,
        acknowledgement: TestnetAcknowledgement,
    ) -> Result<Self, ExecutionError> {
        let account_address = account_address.into();
        validate_address(&account_address)?;
        if action_ttl_ms == 0 || action_ttl_ms > 60_000 {
            return Err(ExecutionError::Validation(
                "testnet action TTL must be in 1..=60000ms".to_owned(),
            ));
        }
        Ok(Self {
            authorized_signer_address: account_address.clone(),
            account_address,
            action_ttl_ms,
            _acknowledgement: acknowledgement,
        })
    }

    /// Public trading account address used for account queries.
    pub fn account_address(&self) -> &str {
        &self.account_address
    }

    /// Pins an API/agent wallet that is authorized for the trading account.
    ///
    /// Hyperliquid agent wallets sign with an address distinct from the public
    /// trading account used for account queries. The host must verify that
    /// authorization independently before constructing the executor.
    ///
    /// # Errors
    ///
    /// Returns an error unless the signer address is canonical lowercase hex.
    pub fn with_authorized_signer_address(
        mut self,
        signer_address: impl Into<String>,
    ) -> Result<Self, ExecutionError> {
        let signer_address = signer_address.into();
        validate_address(&signer_address)?;
        self.authorized_signer_address = signer_address;
        Ok(self)
    }

    /// Authorized agent-wallet signer address, when one has been pinned.
    pub fn authorized_signer_address(&self) -> &str {
        &self.authorized_signer_address
    }
}

/// Secret-free Hyperliquid L1 action that a provider must sign exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperliquidSigningRequest {
    network: ExecutionNetwork,
    nonce: u64,
    expires_after: u64,
    action: Value,
}

impl HyperliquidSigningRequest {
    /// Network whose L1 signing domain must be used.
    pub const fn network(&self) -> ExecutionNetwork {
        self.network
    }

    /// Monotonic request nonce in Unix epoch milliseconds.
    pub const fn nonce(&self) -> u64 {
        self.nonce
    }

    /// Request expiry in Unix epoch milliseconds.
    pub const fn expires_after(&self) -> u64 {
        self.expires_after
    }

    /// Exact JSON action that must appear in the signed request.
    pub const fn action(&self) -> &Value {
        &self.action
    }
}

/// Hyperliquid-aware L1 signer boundary.
///
/// Unlike a generic byte-signing callback, a provider receives the explicit
/// action, nonce, expiry, and network required by Hyperliquid's msgpack and
/// EIP-712 signing scheme. It returns the complete signed exchange request so
/// Hypercarry can verify that none of those fields changed.
pub trait HyperliquidL1Signer {
    /// Error type returned by the signing provider.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Network for which the provider key is authorized.
    fn network(&self) -> ExecutionNetwork;

    /// Non-secret provider/key alias suitable for audit logs.
    fn key_id(&self) -> &str;

    /// Lowercase address of the API/master wallet selected by the provider.
    fn signer_address(&self) -> &str;

    /// Signs one exact Hyperliquid L1 action request.
    ///
    /// # Errors
    ///
    /// Returns a provider error without exposing key material.
    fn sign_l1_action(&self, request: &HyperliquidSigningRequest) -> Result<Value, Self::Error>;
}

/// Secret-free error from the optional hypersdk signer adapter.
#[cfg(feature = "hypersdk-signer")]
#[derive(Debug)]
pub struct HypersdkSignerError(&'static str);

#[cfg(feature = "hypersdk-signer")]
impl fmt::Display for HypersdkSignerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

#[cfg(feature = "hypersdk-signer")]
impl Error for HypersdkSignerError {}

/// Execution-only adapter around a caller-constructed hypersdk-compatible
/// signer. Hypercarry never loads or parses its key material.
#[cfg(feature = "hypersdk-signer")]
pub struct HypersdkTestnetSigner<S> {
    signer: S,
    key_id: String,
    signer_address: String,
}

#[cfg(feature = "hypersdk-signer")]
impl<S> HypersdkTestnetSigner<S>
where
    S: alloy::signers::SignerSync + alloy::signers::Signer,
{
    /// Wraps a signer already created by the host application.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid audit alias.
    pub fn new(signer: S, key_id: impl Into<String>) -> Result<Self, ExecutionError> {
        let key_id = key_id.into();
        validate_provider_alias(&key_id)?;
        let signer_address = signer.address().to_string().to_ascii_lowercase();
        validate_address(&signer_address)?;
        Ok(Self {
            signer,
            key_id,
            signer_address,
        })
    }
}

#[cfg(feature = "hypersdk-signer")]
impl<S> HyperliquidL1Signer for HypersdkTestnetSigner<S>
where
    S: alloy::signers::SignerSync + alloy::signers::Signer,
{
    type Error = HypersdkSignerError;

    fn network(&self) -> ExecutionNetwork {
        ExecutionNetwork::Testnet
    }

    fn key_id(&self) -> &str {
        &self.key_id
    }

    fn signer_address(&self) -> &str {
        &self.signer_address
    }

    fn sign_l1_action(&self, request: &HyperliquidSigningRequest) -> Result<Value, Self::Error> {
        if request.network() != ExecutionNetwork::Testnet {
            return Err(HypersdkSignerError("hypersdk signer is testnet-only"));
        }
        let action: hypersdk::hypercore::Action = serde_json::from_value(request.action().clone())
            .map_err(|_| HypersdkSignerError("unsupported Hyperliquid action"))?;
        let expires_after_ms = i64::try_from(request.expires_after())
            .map_err(|_| HypersdkSignerError("action expiry exceeds i64"))?;
        let expires_after = chrono::DateTime::from_timestamp_millis(expires_after_ms)
            .ok_or(HypersdkSignerError("action expiry is invalid"))?;
        let signed = action
            .sign_sync(
                &self.signer,
                request.nonce(),
                None,
                Some(expires_after),
                hypersdk::hypercore::Chain::Testnet,
            )
            .map_err(|_| HypersdkSignerError("hypersdk L1 signing failed"))?;
        serde_json::to_value(signed)
            .map_err(|_| HypersdkSignerError("hypersdk request serialization failed"))
    }
}

/// Clock provider for deterministic nonce/expiry tests.
pub trait Clock {
    /// Returns current Unix epoch milliseconds.
    ///
    /// # Errors
    ///
    /// Returns an error when trustworthy time is unavailable.
    fn now_ms(&self) -> Result<i64, ExecutionError>;
}

/// Production wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> Result<i64, ExecutionError> {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ExecutionError::Reliability("system clock precedes epoch".to_owned()))?
            .as_millis();
        i64::try_from(millis)
            .map_err(|_| ExecutionError::Reliability("system time exceeds i64".to_owned()))
    }
}

/// Resolves the current Hyperliquid numeric asset index for a validated order.
pub trait HyperliquidAssetResolver {
    /// Returns a perp/spot asset index from fresh metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when the mapping is missing, stale, or ambiguous.
    fn asset_id(&self, order: &ValidatedOrder) -> Result<u32, ExecutionError>;
}

/// Secret-free transport failure classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportFailure {
    /// How the failure should be handled by retry/reconciliation logic.
    pub classification: SubmissionFailure,
    /// Transport operation that failed.
    pub operation: &'static str,
    /// HTTP status code, when the failure carried one.
    pub status: Option<u16>,
}

impl fmt::Display for TransportFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} failed ({:?}, status {:?})",
            self.operation, self.classification, self.status
        )
    }
}

impl Error for TransportFailure {}

/// Injectable REST boundary. Normal tests use scripted responses.
pub trait HyperliquidTransport {
    /// Sends a signed `/exchange` request.
    ///
    /// # Errors
    ///
    /// Returns a classified, secret-free transport failure.
    fn exchange(&mut self, request: &Value) -> Result<Value, TransportFailure>;

    /// Sends an unsigned private-account query to `/info`.
    ///
    /// # Errors
    ///
    /// Returns a classified, secret-free transport failure.
    fn info(&mut self, request: &Value) -> Result<Value, TransportFailure>;
}

/// Bounded blocking HTTPS transport for the official testnet endpoint only.
pub struct ReqwestHyperliquidTransport {
    client: Client,
}

impl ReqwestHyperliquidTransport {
    /// Creates a Rustls client with bounded connect and complete-request time.
    ///
    /// # Errors
    ///
    /// Returns an error if the client cannot be constructed.
    pub fn new(
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, ExecutionError> {
        if connect_timeout.is_zero() || request_timeout.is_zero() {
            return Err(ExecutionError::Validation(
                "Hyperliquid transport timeouts must be positive".to_owned(),
            ));
        }
        let client = Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .map_err(|_| {
                ExecutionError::Transport("could not construct HTTPS client".to_owned())
            })?;
        Ok(Self { client })
    }

    fn post(
        &self,
        operation: &'static str,
        path: &str,
        request: &Value,
    ) -> Result<Value, TransportFailure> {
        let result = self
            .client
            .post(format!("{TESTNET_API}{path}"))
            .json(request)
            .send();
        let response = result.map_err(|error| TransportFailure {
            classification: if error.is_connect() {
                SubmissionFailure::TransientBeforeWrite
            } else {
                SubmissionFailure::UncertainAfterWrite
            },
            operation,
            status: error.status().map(|status| status.as_u16()),
        })?;
        let status = response.status();
        if !status.is_success() {
            let retry_after_ms = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .and_then(|seconds| seconds.checked_mul(1_000));
            return Err(TransportFailure {
                classification: if status == StatusCode::TOO_MANY_REQUESTS {
                    SubmissionFailure::RateLimited { retry_after_ms }
                } else if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
                    SubmissionFailure::UncertainAfterWrite
                } else {
                    SubmissionFailure::Permanent
                },
                operation,
                status: Some(status.as_u16()),
            });
        }
        let bytes = response.bytes().map_err(|_| TransportFailure {
            classification: SubmissionFailure::UncertainAfterWrite,
            operation,
            status: Some(status.as_u16()),
        })?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(TransportFailure {
                classification: SubmissionFailure::Permanent,
                operation,
                status: Some(status.as_u16()),
            });
        }
        serde_json::from_slice(&bytes).map_err(|_| TransportFailure {
            classification: SubmissionFailure::Permanent,
            operation,
            status: Some(status.as_u16()),
        })
    }
}

impl HyperliquidTransport for ReqwestHyperliquidTransport {
    fn exchange(&mut self, request: &Value) -> Result<Value, TransportFailure> {
        self.post("exchange", "/exchange", request)
    }

    fn info(&mut self, request: &Value) -> Result<Value, TransportFailure> {
        self.post("order_status", "/info", request)
    }
}

/// Normalized private order/fill observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivateEvent {
    /// An order lifecycle update.
    OrderUpdate {
        /// Stable identity used to deduplicate the event.
        event_id: String,
        /// Durable client order identifier.
        client_order_id: ClientOrderId,
        /// Venue-assigned order identifier.
        venue_order_id: u64,
        /// Observed lifecycle state.
        state: OrderState,
        /// Cumulative filled quantity at this update.
        cumulative_filled: Decimal,
        /// Event time, unix milliseconds.
        occurred_at_ms: i64,
    },
    /// A fill applied to an order.
    Fill {
        /// Stable identity used to deduplicate the event.
        event_id: String,
        /// Venue-assigned order identifier.
        venue_order_id: u64,
        /// Filled quantity in base-asset units.
        quantity: Decimal,
        /// Event time, unix milliseconds.
        occurred_at_ms: i64,
    },
}

impl PrivateEvent {
    fn event_id(&self) -> &str {
        match self {
            Self::OrderUpdate { event_id, .. } | Self::Fill { event_id, .. } => event_id,
        }
    }

    fn occurred_at_ms(&self) -> i64 {
        match self {
            Self::OrderUpdate { occurred_at_ms, .. } | Self::Fill { occurred_at_ms, .. } => {
                *occurred_at_ms
            }
        }
    }
}

/// Result of applying a private event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateEventOutcome {
    /// The event advanced tracked lifecycle state.
    Applied,
    /// The event was an exact duplicate and ignored.
    Duplicate,
    /// The event referred to an order this executor does not manage.
    Unrelated,
}

/// One receive result from the dedicated private WebSocket connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivateStreamEvent {
    /// A batch of decoded private events.
    Events(Vec<PrivateEvent>),
    /// The private connection dropped.
    Disconnected,
}

/// Connected private stream. It owns no public L2 subscription or recorder.
pub struct PrivateStream<C> {
    connection: C,
}

impl<C: MarketConnection> PrivateStream<C> {
    /// Reads one meaningful private stream boundary while servicing pings.
    ///
    /// # Errors
    ///
    /// Returns a secret-free transport or schema error. Callers must reconnect
    /// and REST-reconcile all managed orders after disconnect.
    pub async fn next_event(&mut self) -> Result<PrivateStreamEvent, ExecutionError> {
        loop {
            match self.connection.next_message().await.map_err(|_| {
                ExecutionError::Transport("private WebSocket receive failed".to_owned())
            })? {
                Some(TransportMessage::Text(text)) => {
                    let message: Value = serde_json::from_str(&text)?;
                    return Ok(PrivateStreamEvent::Events(parse_private_message(&message)?));
                }
                Some(TransportMessage::Ping(payload)) => {
                    self.connection.send_pong(payload).await.map_err(|_| {
                        ExecutionError::Transport("private WebSocket pong failed".to_owned())
                    })?;
                }
                Some(TransportMessage::Pong) => {}
                Some(TransportMessage::Close(_)) | None => {
                    return Ok(PrivateStreamEvent::Disconnected);
                }
            }
        }
    }

    /// Closes the private connection cleanly.
    ///
    /// # Errors
    ///
    /// Returns an error when the close frame cannot be sent.
    pub async fn close(&mut self) -> Result<(), ExecutionError> {
        self.connection
            .close()
            .await
            .map_err(|_| ExecutionError::Transport("private WebSocket close failed".to_owned()))
    }
}

/// Connects and subscribes a dedicated official testnet private stream.
///
/// # Errors
///
/// Returns an error for invalid account identity, connection failure, or a
/// failed subscription write.
pub async fn connect_testnet_private_stream(
    account_address: &str,
) -> Result<PrivateStream<HyperliquidConnection>, ExecutionError> {
    let subscriptions = private_subscription_requests(account_address)?;
    let transport = HyperliquidWebSocketTransport::for_network(Network::Testnet);
    let mut connection = transport.connect().await.map_err(|_| {
        ExecutionError::Transport("private testnet WebSocket connect failed".to_owned())
    })?;
    for subscription in subscriptions {
        connection
            .send_text(serde_json::to_string(&subscription)?)
            .await
            .map_err(|_| {
                ExecutionError::Transport("private subscription write failed".to_owned())
            })?;
    }
    Ok(PrivateStream { connection })
}

/// One recovered, managed testnet order.
pub struct LiveOrder {
    machine: OrderStateMachine,
    seen_private_events: BTreeSet<String>,
    rejection: Option<Rejection>,
}

impl LiveOrder {
    fn new(machine: OrderStateMachine) -> Self {
        Self {
            machine,
            seen_private_events: BTreeSet::new(),
            rejection: None,
        }
    }

    /// Recovers lifecycle and private-event deduplication state from a journal.
    ///
    /// # Errors
    ///
    /// Returns an error when durable transitions do not replay exactly.
    pub fn recover(
        order: ValidatedOrder,
        client_order_id: ClientOrderId,
        events: &[JournalEvent],
    ) -> Result<Self, ExecutionError> {
        let correlation_id = order.correlation_id.clone();
        let machine = OrderStateMachine::recover_from_journal(order, client_order_id, events)?;
        let seen_private_events = events
            .iter()
            .filter(|event| event.correlation_id == correlation_id)
            .filter_map(|event| match &event.event {
                JournalEventKind::ExternalEventApplied { source, event_id }
                    if source == "hyperliquid_private" =>
                {
                    Some(event_id.clone())
                }
                _ => None,
            })
            .collect();
        let rejection = events.iter().rev().find_map(|event| {
            if event.correlation_id != correlation_id {
                return None;
            }
            match &event.event {
                JournalEventKind::RiskRejected { rejection }
                | JournalEventKind::VenueRejected { rejection } => Some(rejection.clone()),
                _ => None,
            }
        });
        Ok(Self {
            machine,
            seen_private_events,
            rejection,
        })
    }

    /// Current lifecycle state.
    pub const fn state(&self) -> OrderState {
        self.machine.state()
    }

    /// Durable client order identifier.
    pub const fn client_order_id(&self) -> ClientOrderId {
        self.machine.client_order_id()
    }

    /// Venue-assigned order identifier, once known.
    pub const fn venue_order_id(&self) -> Option<u64> {
        self.machine.venue_order_id()
    }

    /// Cumulative filled quantity observed so far.
    pub const fn cumulative_filled(&self) -> Decimal {
        self.machine.cumulative_filled()
    }

    /// The validated order being tracked.
    pub fn order(&self) -> &ValidatedOrder {
        self.machine.order()
    }

    /// Snapshot the current lifecycle as an execution report.
    pub fn report(&self) -> ExecutionReport {
        let mut report = ExecutionReport::empty(
            self.machine.order(),
            ExecutionMode::Testnet,
            self.machine.state(),
        );
        report.rejection.clone_from(&self.rejection);
        report
    }
}

/// Proactive throttling and bounded retry settings supplied as one dependency.
pub struct TestnetReliability {
    throttle: RequestThrottle,
    retry: RetryPolicy,
}

impl TestnetReliability {
    /// Bundle a request throttle and retry policy for the executor.
    pub fn new(throttle: RequestThrottle, retry: RetryPolicy) -> Self {
        Self { throttle, retry }
    }
}

/// Feature-gated, testnet-only signed lifecycle executor.
pub struct HyperliquidTestnetExecutor<S, T, J, C, A> {
    config: HyperliquidTestnetConfig,
    signer: S,
    transport: T,
    journal: J,
    clock: C,
    assets: A,
    throttle: RequestThrottle,
    retry: RetryPolicy,
    last_nonce: Option<i64>,
}

impl<S, T, J, C, A> HyperliquidTestnetExecutor<S, T, J, C, A>
where
    S: HyperliquidL1Signer,
    T: HyperliquidTransport,
    J: Journal,
    C: Clock,
    A: HyperliquidAssetResolver,
{
    /// Constructs an executor only for a validated testnet signer/configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for a signer network mismatch or invalid alias.
    pub fn new(
        config: HyperliquidTestnetConfig,
        signer: S,
        transport: T,
        journal: J,
        clock: C,
        assets: A,
        reliability: TestnetReliability,
    ) -> Result<Self, ExecutionError> {
        validate_l1_signer(&signer, &config)?;
        Ok(Self {
            config,
            signer,
            transport,
            journal,
            clock,
            assets,
            throttle: reliability.throttle,
            retry: reliability.retry,
            last_nonce: None,
        })
    }

    /// Runs metadata and risk gates, journals the exact action, then signs and
    /// submits one GTC limit order to Hyperliquid testnet.
    ///
    /// # Errors
    ///
    /// Returns an error before signing for metadata, policy, journal, throttle,
    /// serialization, or provider failures. Uncertain submissions are always
    /// reconciled by client order ID before this method returns.
    pub fn place<R, P>(
        &mut self,
        intent: &OrderIntent,
        metadata: &R,
        policy: &P,
    ) -> Result<LiveOrder, ExecutionError>
    where
        R: MarketMetadataResolver,
        P: RiskPolicy,
    {
        let metadata = metadata.resolve(&intent.venue, &intent.market)?;
        let order = ValidatedOrder::resolve(intent, &metadata)?;
        let client_order_id = ClientOrderId::derive(&order)?;
        let decision = policy.evaluate(&order)?;
        self.journal.append(
            intent.created_at_ms,
            &intent.correlation_id,
            decision.clone().into(),
        )?;
        let mut live = LiveOrder::new(OrderStateMachine::new(order.clone(), client_order_id));
        if let RiskDecision::Reject { code, reason } = decision {
            live.rejection = Some(Rejection { code, reason });
            self.apply_transition(
                &mut live,
                OrderState::Rejected,
                intent.created_at_ms,
                None,
                Decimal::ZERO,
            )?;
            return Ok(live);
        }
        self.journal.append(
            intent.created_at_ms,
            &intent.correlation_id,
            JournalEventKind::ExactAction {
                mode: ExecutionMode::Testnet,
                order: order.clone(),
            },
        )?;
        let asset = self.assets.asset_id(&order)?;
        let action = HyperliquidAction::Order(OrderAction {
            kind: "order",
            orders: vec![OrderWire {
                asset,
                is_buy: order.side == Side::Buy,
                price: decimal_wire(order.limit_price),
                size: decimal_wire(order.quantity),
                reduce_only: false,
                order_type: OrderType::gtc(),
                client_order_id,
            }],
            grouping: "na",
        });
        let (request, nonce) = self.signed_request(&action)?;
        // Signing can take an unbounded amount of wall-clock time (an
        // interactive external signer, a remote HSM round-trip). Re-check the
        // process-independent kill switch here, immediately before the
        // irreversible submit, so an emergency stop engaged during signing
        // still stops this not-yet-sent order instead of only the next one.
        if policy.kill_switch_engaged()? {
            let rejection = Rejection {
                code: "kill_switch".to_owned(),
                reason: "process-independent kill switch engaged before submission".to_owned(),
            };
            self.journal.append(
                nonce,
                &order.correlation_id,
                JournalEventKind::RiskRejected {
                    rejection: rejection.clone(),
                },
            )?;
            live.rejection = Some(rejection);
            self.apply_transition(&mut live, OrderState::Rejected, nonce, None, Decimal::ZERO)?;
            return Ok(live);
        }
        self.apply_transition(
            &mut live,
            OrderState::SubmissionPending,
            nonce,
            None,
            Decimal::ZERO,
        )?;
        match self.transport.exchange(&request) {
            Ok(response) => self.apply_placement_response(&mut live, nonce, &response)?,
            Err(failure) => self.handle_submission_failure(&mut live, nonce, &failure)?,
        }
        Ok(live)
    }

    /// Sends an idempotent cancellation by client order ID.
    ///
    /// A cancellation acknowledgement is not treated as final. The order stays
    /// `cancel_pending` until a private event or REST reconciliation establishes
    /// the race outcome.
    ///
    /// # Errors
    ///
    /// Returns an error for terminal orders, signing/journal failures, or a
    /// definitive transport failure.
    pub fn cancel(&mut self, live: &mut LiveOrder) -> Result<(), ExecutionError> {
        if matches!(
            live.state(),
            OrderState::Filled | OrderState::Cancelled | OrderState::Rejected
        ) {
            return Err(ExecutionError::Lifecycle(
                "cannot cancel a terminal order".to_owned(),
            ));
        }
        let asset = self.assets.asset_id(live.order())?;
        let action = HyperliquidAction::Cancel(CancelAction {
            kind: "cancelByCloid",
            cancels: vec![CancelWire {
                asset,
                cloid: live.client_order_id(),
            }],
        });
        let (request, nonce) = self.signed_request(&action)?;
        self.apply_transition(
            live,
            OrderState::CancelPending,
            nonce,
            live.venue_order_id(),
            live.cumulative_filled(),
        )?;
        match self.transport.exchange(&request) {
            Ok(response) => {
                if let Some(reason) = exchange_error(&response)? {
                    let state = if live.cumulative_filled().is_zero() {
                        OrderState::Open
                    } else {
                        OrderState::PartiallyFilled
                    };
                    self.apply_transition(
                        live,
                        state,
                        nonce,
                        live.venue_order_id(),
                        live.cumulative_filled(),
                    )?;
                    return Err(ExecutionError::Transport(format!(
                        "testnet cancellation rejected: {reason}"
                    )));
                }
                validate_cancel_response(&response)?;
            }
            Err(failure)
                if matches!(
                    failure.classification,
                    SubmissionFailure::UncertainAfterWrite
                ) =>
            {
                self.reconcile(live)?;
            }
            Err(failure) => return Err(transport_error(&failure)),
        }
        Ok(())
    }

    /// Reconciles one managed order through REST by client order ID.
    ///
    /// # Errors
    ///
    /// Returns an error for transport/schema failures or an impossible venue
    /// transition. A not-yet-visible uncertain order remains uncertain.
    pub fn reconcile(&mut self, live: &mut LiveOrder) -> Result<(), ExecutionError> {
        self.admit_request()?;
        let request = json!({
            "type": "orderStatus",
            "user": self.config.account_address(),
            "oid": live.client_order_id(),
        });
        let response = self
            .transport
            .info(&request)
            .map_err(|failure| transport_error(&failure))?;
        let Some(status) = parse_order_status(&response, live.order().quantity)? else {
            if live.state() == OrderState::SubmissionUncertain {
                return Ok(());
            }
            return Err(ExecutionError::Lifecycle(
                "managed order disappeared from REST reconciliation".to_owned(),
            ));
        };
        self.apply_transition(
            live,
            status.state,
            status.occurred_at_ms,
            status.venue_order_id,
            status.cumulative_filled,
        )?;
        Ok(())
    }

    /// Applies a normalized private order/fill event idempotently.
    ///
    /// # Errors
    ///
    /// Returns an error for identity mismatches or impossible lifecycle data.
    pub fn apply_private_event(
        &mut self,
        live: &mut LiveOrder,
        event: &PrivateEvent,
    ) -> Result<PrivateEventOutcome, ExecutionError> {
        if live.seen_private_events.contains(event.event_id()) {
            return Ok(PrivateEventOutcome::Duplicate);
        }
        let (state, venue_order_id, cumulative_filled) = match event {
            PrivateEvent::OrderUpdate {
                client_order_id,
                venue_order_id,
                state,
                cumulative_filled,
                ..
            } => {
                if *client_order_id != live.client_order_id() {
                    return Ok(PrivateEventOutcome::Unrelated);
                }
                (*state, Some(*venue_order_id), *cumulative_filled)
            }
            PrivateEvent::Fill {
                venue_order_id,
                quantity,
                ..
            } => {
                if live
                    .venue_order_id()
                    .is_some_and(|oid| oid != *venue_order_id)
                {
                    return Ok(PrivateEventOutcome::Unrelated);
                }
                let cumulative =
                    live.cumulative_filled()
                        .checked_add(*quantity)
                        .ok_or_else(|| {
                            ExecutionError::Lifecycle("fill quantity overflow".to_owned())
                        })?;
                let state = if cumulative == live.order().quantity {
                    OrderState::Filled
                } else {
                    OrderState::PartiallyFilled
                };
                (state, Some(*venue_order_id), cumulative)
            }
        };
        self.apply_transition(
            live,
            state,
            event.occurred_at_ms(),
            venue_order_id,
            cumulative_filled,
        )?;
        self.journal.append(
            event.occurred_at_ms(),
            &live.order().correlation_id,
            JournalEventKind::ExternalEventApplied {
                source: "hyperliquid_private".to_owned(),
                event_id: event.event_id().to_owned(),
            },
        )?;
        live.seen_private_events.insert(event.event_id().to_owned());
        Ok(PrivateEventOutcome::Applied)
    }

    /// Borrow the durable execution journal.
    pub fn journal(&self) -> &J {
        &self.journal
    }

    /// Decompose the executor into its transport and journal.
    pub fn into_parts(self) -> (T, J) {
        (self.transport, self.journal)
    }

    fn signed_request(
        &mut self,
        action: &HyperliquidAction,
    ) -> Result<(Value, i64), ExecutionError> {
        self.admit_request()?;
        let nonce = self.next_nonce()?;
        let expires_after = nonce
            .checked_add(i64::try_from(self.config.action_ttl_ms).map_err(|_| {
                ExecutionError::Reliability("action TTL exceeds signed time".to_owned())
            })?)
            .ok_or_else(|| ExecutionError::Reliability("action expiry overflow".to_owned()))?;
        let request = HyperliquidSigningRequest {
            network: ExecutionNetwork::Testnet,
            nonce: u64::try_from(nonce)
                .map_err(|_| ExecutionError::Reliability("nonce is negative".to_owned()))?,
            expires_after: u64::try_from(expires_after)
                .map_err(|_| ExecutionError::Reliability("action expiry is negative".to_owned()))?,
            action: serde_json::to_value(action)?,
        };
        let signed = self.signer.sign_l1_action(&request).map_err(|_| {
            ExecutionError::Transport(format!(
                "isolated signer provider `{}` failed",
                self.signer.key_id()
            ))
        })?;
        validate_signed_request(&signed, &request)?;
        Ok((signed, nonce))
    }

    fn next_nonce(&mut self) -> Result<i64, ExecutionError> {
        let now = self.clock.now_ms()?;
        if now < 0 {
            return Err(ExecutionError::Reliability(
                "clock returned a pre-epoch nonce".to_owned(),
            ));
        }
        let nonce = match self.last_nonce {
            Some(last) if now <= last => last
                .checked_add(1)
                .ok_or_else(|| ExecutionError::Reliability("nonce overflow".to_owned()))?,
            _ => now,
        };
        self.last_nonce = Some(nonce);
        Ok(nonce)
    }

    fn admit_request(&mut self) -> Result<(), ExecutionError> {
        let now = self.clock.now_ms()?;
        match self.throttle.admit(now)? {
            ThrottleDecision::Allow => Ok(()),
            ThrottleDecision::Wait { retry_after_ms } => Err(ExecutionError::Reliability(format!(
                "proactive request throttle requires {retry_after_ms}ms delay"
            ))),
        }
    }

    fn apply_placement_response(
        &mut self,
        live: &mut LiveOrder,
        occurred_at_ms: i64,
        response: &Value,
    ) -> Result<(), ExecutionError> {
        match parse_placement(response, live.order().quantity)? {
            PlacementStatus::Accepted {
                state,
                venue_order_id,
                cumulative_filled,
            } => self.apply_transition(
                live,
                state,
                occurred_at_ms,
                venue_order_id,
                cumulative_filled,
            ),
            PlacementStatus::Rejected { code, reason } => {
                let rejection = Rejection { code, reason };
                self.journal.append(
                    occurred_at_ms,
                    &live.order().correlation_id,
                    JournalEventKind::VenueRejected {
                        rejection: rejection.clone(),
                    },
                )?;
                live.rejection = Some(rejection);
                self.apply_transition(
                    live,
                    OrderState::Rejected,
                    occurred_at_ms,
                    None,
                    Decimal::ZERO,
                )
            }
        }
    }

    fn handle_submission_failure(
        &mut self,
        live: &mut LiveOrder,
        occurred_at_ms: i64,
        failure: &TransportFailure,
    ) -> Result<(), ExecutionError> {
        match self.retry.action(1, failure.classification) {
            RecoveryAction::ReconcileBeforeRetry => {
                self.apply_transition(
                    live,
                    OrderState::SubmissionUncertain,
                    occurred_at_ms,
                    None,
                    Decimal::ZERO,
                )?;
                self.reconcile(live)
            }
            RecoveryAction::RetryAfter { delay_ms } => Err(ExecutionError::Reliability(format!(
                "submission retry requires explicit {delay_ms}ms wait"
            ))),
            RecoveryAction::Stop => Err(transport_error(failure)),
        }
    }

    fn apply_transition(
        &mut self,
        live: &mut LiveOrder,
        state: OrderState,
        occurred_at_ms: i64,
        venue_order_id: Option<u64>,
        cumulative_filled: Decimal,
    ) -> Result<(), ExecutionError> {
        match live
            .machine
            .transition(state, occurred_at_ms, venue_order_id, cumulative_filled)?
        {
            TransitionOutcome::Applied(transition) => {
                self.append_lifecycle(live.order(), transition)
            }
            TransitionOutcome::Duplicate => Ok(()),
        }
    }

    fn append_lifecycle(
        &mut self,
        order: &ValidatedOrder,
        transition: LifecycleTransition,
    ) -> Result<(), ExecutionError> {
        self.journal.append(
            transition.occurred_at_ms,
            &order.correlation_id,
            JournalEventKind::LifecycleTransition { transition },
        )?;
        Ok(())
    }
}

/// Returns the private WebSocket subscriptions kept separate from public L2.
///
/// # Errors
///
/// Returns an error unless the account is a canonical lowercase address.
pub fn private_subscription_requests(account_address: &str) -> Result<[Value; 2], ExecutionError> {
    validate_address(account_address)?;
    Ok([
        json!({
            "method": "subscribe",
            "subscription": {"type": "orderUpdates", "user": account_address},
        }),
        json!({
            "method": "subscribe",
            "subscription": {"type": "userFills", "user": account_address},
        }),
    ])
}

/// Parses only the private `orderUpdates` and `userFills` channels.
///
/// Subscription acknowledgements and unrelated channels return an empty list.
/// Snapshot fills are retained and deduplicated by trade ID during application,
/// which closes disconnect gaps without double-counting.
///
/// # Errors
///
/// Returns an error for a recognized private channel with malformed identity,
/// timestamp, size, or status fields.
pub fn parse_private_message(message: &Value) -> Result<Vec<PrivateEvent>, ExecutionError> {
    match message.get("channel").and_then(Value::as_str) {
        Some("orderUpdates") => parse_order_updates(
            message
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| venue_schema("orderUpdates data must be an array"))?,
        ),
        Some("userFills") => parse_user_fills(
            message
                .pointer("/data/fills")
                .and_then(Value::as_array)
                .ok_or_else(|| venue_schema("userFills data.fills must be an array"))?,
        ),
        _ => Ok(Vec::new()),
    }
}

fn parse_order_updates(updates: &[Value]) -> Result<Vec<PrivateEvent>, ExecutionError> {
    updates
        .iter()
        .map(|update| {
            let order = update
                .get("order")
                .ok_or_else(|| venue_schema("order update has no order"))?;
            let client_order_id = order
                .get("cloid")
                .and_then(Value::as_str)
                .ok_or_else(|| venue_schema("order update has no cloid"))?
                .parse()?;
            let venue_order_id = order
                .get("oid")
                .and_then(Value::as_u64)
                .ok_or_else(|| venue_schema("order update has no oid"))?;
            let original = order
                .get("origSz")
                .and_then(Value::as_str)
                .map(parse_decimal)
                .transpose()?
                .ok_or_else(|| venue_schema("order update has no origSz"))?;
            let remaining = order
                .get("sz")
                .and_then(Value::as_str)
                .map(parse_decimal)
                .transpose()?
                .ok_or_else(|| venue_schema("order update has no sz"))?;
            let cumulative_filled = original
                .checked_sub(remaining)
                .ok_or_else(|| venue_schema("order update fill quantity overflow"))?;
            let status = update
                .get("status")
                .and_then(Value::as_str)
                .ok_or_else(|| venue_schema("order update has no status"))?;
            let state = map_venue_status(status, cumulative_filled, original)?;
            let occurred_at_ms = update
                .get("statusTimestamp")
                .and_then(Value::as_i64)
                .or_else(|| order.get("timestamp").and_then(Value::as_i64))
                .ok_or_else(|| venue_schema("order update has no timestamp"))?;
            Ok(PrivateEvent::OrderUpdate {
                event_id: format!(
                    "order:{venue_order_id}:{status}:{occurred_at_ms}:{}",
                    decimal_wire(remaining)
                ),
                client_order_id,
                venue_order_id,
                state,
                cumulative_filled,
                occurred_at_ms,
            })
        })
        .collect()
}

fn parse_user_fills(fills: &[Value]) -> Result<Vec<PrivateEvent>, ExecutionError> {
    fills
        .iter()
        .map(|fill| {
            let trade_id = fill
                .get("tid")
                .and_then(Value::as_u64)
                .ok_or_else(|| venue_schema("private fill has no trade ID"))?;
            let venue_order_id = fill
                .get("oid")
                .and_then(Value::as_u64)
                .ok_or_else(|| venue_schema("private fill has no order ID"))?;
            let quantity = fill
                .get("sz")
                .and_then(Value::as_str)
                .map(parse_decimal)
                .transpose()?
                .ok_or_else(|| venue_schema("private fill has no size"))?;
            if quantity <= Decimal::ZERO {
                return Err(venue_schema("private fill size must be positive"));
            }
            let occurred_at_ms = fill
                .get("time")
                .and_then(Value::as_i64)
                .ok_or_else(|| venue_schema("private fill has no timestamp"))?;
            Ok(PrivateEvent::Fill {
                event_id: format!("fill:{trade_id}"),
                venue_order_id,
                quantity,
                occurred_at_ms,
            })
        })
        .collect()
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum HyperliquidAction {
    Order(OrderAction),
    Cancel(CancelAction),
}

#[derive(Debug, Serialize)]
struct OrderAction {
    #[serde(rename = "type")]
    kind: &'static str,
    orders: Vec<OrderWire>,
    grouping: &'static str,
}

#[derive(Debug, Serialize)]
struct OrderWire {
    #[serde(rename = "a")]
    asset: u32,
    #[serde(rename = "b")]
    is_buy: bool,
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "s")]
    size: String,
    #[serde(rename = "r")]
    reduce_only: bool,
    #[serde(rename = "t")]
    order_type: OrderType,
    #[serde(rename = "c")]
    client_order_id: ClientOrderId,
}

#[derive(Debug, Serialize)]
struct OrderType {
    limit: LimitOrderType,
}

impl OrderType {
    const fn gtc() -> Self {
        Self {
            limit: LimitOrderType { tif: "Gtc" },
        }
    }
}

#[derive(Debug, Serialize)]
struct LimitOrderType {
    tif: &'static str,
}

#[derive(Debug, Serialize)]
struct CancelAction {
    #[serde(rename = "type")]
    kind: &'static str,
    cancels: Vec<CancelWire>,
}

#[derive(Debug, Serialize)]
struct CancelWire {
    asset: u32,
    cloid: ClientOrderId,
}

#[derive(Debug)]
enum PlacementStatus {
    Accepted {
        state: OrderState,
        venue_order_id: Option<u64>,
        cumulative_filled: Decimal,
    },
    Rejected {
        code: String,
        reason: String,
    },
}

struct ReconciledStatus {
    state: OrderState,
    venue_order_id: Option<u64>,
    cumulative_filled: Decimal,
    occurred_at_ms: i64,
}

fn parse_placement(
    response: &Value,
    order_quantity: Decimal,
) -> Result<PlacementStatus, ExecutionError> {
    if let Some(reason) = exchange_error(response)? {
        return Ok(PlacementStatus::Rejected {
            code: "venue_rejected".to_owned(),
            reason,
        });
    }
    let status = response
        .pointer("/response/data/statuses/0")
        .ok_or_else(|| venue_schema("placement response has no first status"))?;
    if let Some(oid) = status.pointer("/resting/oid").and_then(Value::as_u64) {
        return Ok(PlacementStatus::Accepted {
            state: OrderState::Open,
            venue_order_id: Some(oid),
            cumulative_filled: Decimal::ZERO,
        });
    }
    if let Some(filled) = status.get("filled") {
        let cumulative_filled = filled
            .get("totalSz")
            .and_then(Value::as_str)
            .map(parse_decimal)
            .transpose()?
            .unwrap_or(order_quantity);
        return Ok(PlacementStatus::Accepted {
            state: if cumulative_filled == order_quantity {
                OrderState::Filled
            } else {
                OrderState::PartiallyFilled
            },
            venue_order_id: filled.get("oid").and_then(Value::as_u64),
            cumulative_filled,
        });
    }
    if let Some(reason) = status.get("error").and_then(Value::as_str) {
        return Ok(PlacementStatus::Rejected {
            code: "order_rejected".to_owned(),
            reason: bounded_reason(reason),
        });
    }
    Err(venue_schema("unknown placement status"))
}

fn parse_order_status(
    response: &Value,
    order_quantity: Decimal,
) -> Result<Option<ReconciledStatus>, ExecutionError> {
    if response.as_str() == Some("unknownOid")
        || response.get("status").and_then(Value::as_str) == Some("unknownOid")
    {
        return Ok(None);
    }
    let envelope_status = response
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| venue_schema("orderStatus response has no status"))?;
    let (status_text, order, status_timestamp) = if envelope_status == "order" {
        let status_record = response
            .get("order")
            .ok_or_else(|| venue_schema("orderStatus response has no status record"))?;
        let status_text = status_record
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| venue_schema("orderStatus status record has no status"))?;
        let order = status_record
            .get("order")
            .ok_or_else(|| venue_schema("orderStatus status record has no order"))?;
        let timestamp = status_record.get("statusTimestamp").and_then(Value::as_i64);
        (status_text, order, timestamp)
    } else {
        let order = response
            .get("order")
            .ok_or_else(|| venue_schema("orderStatus response has no order"))?;
        let timestamp = response.get("statusTimestamp").and_then(Value::as_i64);
        (envelope_status, order, timestamp)
    };
    let venue_order_id = order.get("oid").and_then(Value::as_u64);
    let original = order
        .get("origSz")
        .and_then(Value::as_str)
        .map(parse_decimal)
        .transpose()?
        .unwrap_or(order_quantity);
    let remaining = order
        .get("sz")
        .and_then(Value::as_str)
        .map(parse_decimal)
        .transpose()?
        .unwrap_or_else(|| {
            if status_text == "filled" {
                Decimal::ZERO
            } else {
                original
            }
        });
    let cumulative_filled = original
        .checked_sub(remaining)
        .ok_or_else(|| venue_schema("orderStatus fill quantity overflow"))?;
    let state = map_venue_status(status_text, cumulative_filled, original)?;
    let occurred_at_ms = status_timestamp
        .or_else(|| order.get("timestamp").and_then(Value::as_i64))
        .ok_or_else(|| venue_schema("orderStatus response has no timestamp"))?;
    Ok(Some(ReconciledStatus {
        state,
        venue_order_id,
        cumulative_filled,
        occurred_at_ms,
    }))
}

fn map_venue_status(
    status: &str,
    cumulative_filled: Decimal,
    original: Decimal,
) -> Result<OrderState, ExecutionError> {
    match status {
        "open" => Ok(if cumulative_filled.is_zero() {
            OrderState::Open
        } else {
            OrderState::PartiallyFilled
        }),
        "filled" if cumulative_filled == original => Ok(OrderState::Filled),
        "canceled"
        | "marginCanceled"
        | "vaultWithdrawalCanceled"
        | "openInterestCapCanceled"
        | "selfTradeCanceled"
        | "reduceOnlyCanceled"
        | "siblingFilledCanceled"
        | "delistedCanceled"
        | "scheduledCancel" => Ok(OrderState::Cancelled),
        "rejected" => Ok(OrderState::Rejected),
        _ => Err(venue_schema(format!("unsupported order status {status:?}"))),
    }
}

fn exchange_error(response: &Value) -> Result<Option<String>, ExecutionError> {
    match response.get("status").and_then(Value::as_str) {
        Some("ok") => Ok(None),
        Some("err") => Ok(Some(bounded_reason(
            response
                .get("response")
                .and_then(Value::as_str)
                .unwrap_or("venue rejected request"),
        ))),
        _ => Err(venue_schema("exchange response has unknown status")),
    }
}

fn validate_cancel_response(response: &Value) -> Result<(), ExecutionError> {
    let status = response
        .pointer("/response/data/statuses/0")
        .and_then(Value::as_str)
        .ok_or_else(|| venue_schema("cancel response has no first status"))?;
    if status != "success" {
        return Err(venue_schema("cancel response was not successful"));
    }
    Ok(())
}

fn validate_signature(signature: &Value) -> Result<(), ExecutionError> {
    let object = signature.as_object().ok_or_else(|| {
        ExecutionError::Transport("signer signature must be a JSON object".to_owned())
    })?;
    let valid_scalar = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .is_some_and(|value| {
                value.len() == 66
                    && value.starts_with("0x")
                    && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
            })
    };
    if !valid_scalar("r")
        || !valid_scalar("s")
        || object
            .get("v")
            .and_then(Value::as_u64)
            .is_none_or(|value| u8::try_from(value).is_err())
    {
        return Err(ExecutionError::Transport(
            "signer signature must contain 32-byte hex r/s and byte v".to_owned(),
        ));
    }
    Ok(())
}

fn validate_signed_request(
    signed: &Value,
    expected: &HyperliquidSigningRequest,
) -> Result<(), ExecutionError> {
    let object = signed.as_object().ok_or_else(|| {
        ExecutionError::Transport("signer response must be a JSON object".to_owned())
    })?;
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "action" | "nonce" | "signature" | "vaultAddress" | "expiresAfter"
        )
    }) {
        return Err(ExecutionError::Transport(
            "signer response contains an unsupported field".to_owned(),
        ));
    }
    if object.get("action") != Some(expected.action())
        || object.get("nonce").and_then(Value::as_u64) != Some(expected.nonce())
        || object.get("expiresAfter").and_then(Value::as_u64) != Some(expected.expires_after())
    {
        return Err(ExecutionError::Transport(
            "signer changed the action, nonce, or expiry".to_owned(),
        ));
    }
    if object
        .get("vaultAddress")
        .is_some_and(|address| !address.is_null())
    {
        return Err(ExecutionError::Transport(
            "vault signing is not supported by the testnet executor".to_owned(),
        ));
    }
    validate_signature(
        object.get("signature").ok_or_else(|| {
            ExecutionError::Transport("signer response has no signature".to_owned())
        })?,
    )
}

fn validate_address(address: &str) -> Result<(), ExecutionError> {
    if address.len() != 42
        || !address.starts_with("0x")
        || !address[2..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(ExecutionError::Validation(
            "account address must be 0x plus 40 lowercase hexadecimal digits".to_owned(),
        ));
    }
    Ok(())
}

fn validate_provider_alias(key_id: &str) -> Result<(), ExecutionError> {
    if key_id.is_empty()
        || key_id.len() > 128
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
    {
        return Err(ExecutionError::Validation(
            "signer key ID must be a bounded non-secret provider alias".to_owned(),
        ));
    }
    Ok(())
}

fn validate_l1_signer<S: HyperliquidL1Signer>(
    signer: &S,
    config: &HyperliquidTestnetConfig,
) -> Result<(), ExecutionError> {
    if signer.network() != ExecutionNetwork::Testnet {
        return Err(ExecutionError::Validation(format!(
            "signer network {} does not match requested testnet",
            signer.network()
        )));
    }
    validate_provider_alias(signer.key_id())?;
    validate_address(signer.signer_address())?;
    if signer.signer_address() != config.authorized_signer_address() {
        return Err(ExecutionError::Validation(
            "signer address does not match the configured authorized signer".to_owned(),
        ));
    }
    Ok(())
}

fn decimal_wire(value: Decimal) -> String {
    value.normalize().to_string()
}

fn parse_decimal(value: &str) -> Result<Decimal, ExecutionError> {
    value
        .parse()
        .map_err(|_| venue_schema("venue returned an invalid decimal"))
}

fn bounded_reason(value: &str) -> String {
    value.chars().take(256).collect()
}

fn transport_error(failure: &TransportFailure) -> ExecutionError {
    ExecutionError::Transport(failure.to_string())
}

fn venue_schema(message: impl Into<String>) -> ExecutionError {
    ExecutionError::Transport(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryJournal, MarketMetadata};
    use std::{cell::Cell, collections::VecDeque, convert::Infallible, rc::Rc};

    struct Resolver;

    impl MarketMetadataResolver for Resolver {
        fn resolve(&self, venue: &str, market: &str) -> Result<MarketMetadata, ExecutionError> {
            MarketMetadata::new(venue, market, dec("0.1"), dec("0.1"), dec("0.1"))
        }
    }

    struct Allow;

    impl RiskPolicy for Allow {
        fn evaluate(&self, _order: &ValidatedOrder) -> Result<RiskDecision, ExecutionError> {
            Ok(RiskDecision::Allow {
                code: "fixture_allow".to_owned(),
                reason: "all fixture limits satisfied".to_owned(),
            })
        }
    }

    struct Reject;

    impl RiskPolicy for Reject {
        fn evaluate(&self, _order: &ValidatedOrder) -> Result<RiskDecision, ExecutionError> {
            Ok(RiskDecision::Reject {
                code: "kill_switch".to_owned(),
                reason: "fixture emergency stop".to_owned(),
            })
        }
    }

    struct AllowThenKillSwitch;

    impl RiskPolicy for AllowThenKillSwitch {
        fn evaluate(&self, _order: &ValidatedOrder) -> Result<RiskDecision, ExecutionError> {
            Ok(RiskDecision::Allow {
                code: "fixture_allow".to_owned(),
                reason: "all fixture limits satisfied".to_owned(),
            })
        }

        fn kill_switch_engaged(&self) -> Result<bool, ExecutionError> {
            Ok(true)
        }
    }

    struct Assets;

    impl HyperliquidAssetResolver for Assets {
        fn asset_id(&self, _order: &ValidatedOrder) -> Result<u32, ExecutionError> {
            Ok(0)
        }
    }

    struct FixtureSigner {
        calls: Rc<Cell<usize>>,
        address: &'static str,
    }

    impl HyperliquidL1Signer for FixtureSigner {
        type Error = Infallible;

        fn network(&self) -> ExecutionNetwork {
            ExecutionNetwork::Testnet
        }

        fn key_id(&self) -> &'static str {
            "fixture/testnet-key"
        }

        fn signer_address(&self) -> &str {
            self.address
        }

        fn sign_l1_action(
            &self,
            request: &HyperliquidSigningRequest,
        ) -> Result<Value, Self::Error> {
            assert_eq!(request.network(), ExecutionNetwork::Testnet);
            self.calls.set(self.calls.get() + 1);
            Ok(json!({
                "action": request.action(),
                "nonce": request.nonce(),
                "signature": {
                    "r": format!("0x{}", "01".repeat(32)),
                    "s": format!("0x{}", "02".repeat(32)),
                    "v": 27
                },
                "vaultAddress": null,
                "expiresAfter": request.expires_after()
            }))
        }
    }

    struct SequenceClock(Cell<i64>);

    impl Clock for SequenceClock {
        fn now_ms(&self) -> Result<i64, ExecutionError> {
            let now = self.0.get();
            self.0.set(now + 1);
            Ok(now)
        }
    }

    #[derive(Default)]
    struct ScriptedTransport {
        exchange: VecDeque<Result<Value, TransportFailure>>,
        info: VecDeque<Result<Value, TransportFailure>>,
        exchange_requests: Vec<Value>,
        info_requests: Vec<Value>,
    }

    impl HyperliquidTransport for ScriptedTransport {
        fn exchange(&mut self, request: &Value) -> Result<Value, TransportFailure> {
            self.exchange_requests.push(request.clone());
            self.exchange
                .pop_front()
                .expect("scripted exchange response")
        }

        fn info(&mut self, request: &Value) -> Result<Value, TransportFailure> {
            self.info_requests.push(request.clone());
            self.info.pop_front().expect("scripted info response")
        }
    }

    #[test]
    fn place_private_partial_fill_cancel_reconcile_and_restart() {
        let calls = Rc::new(Cell::new(0));
        let mut transport = ScriptedTransport::default();
        transport.exchange.push_back(Ok(resting_response(42)));
        transport.exchange.push_back(Ok(cancel_response()));
        transport
            .info
            .push_back(Ok(order_status("canceled", 42, "1", "0.6", 10_006)));
        let mut executor = executor(transport, Rc::clone(&calls));
        let mut live = executor.place(&intent(), &Resolver, &Allow).unwrap();
        assert_eq!(live.state(), OrderState::Open);
        assert_eq!(live.venue_order_id(), Some(42));

        let fill_message = json!({
            "channel": "userFills",
            "data": {
                "isSnapshot": false,
                "fills": [{"tid": 7, "oid": 42, "sz": "0.4", "time": 10002}]
            }
        });
        let fill = parse_private_message(&fill_message).unwrap().remove(0);
        assert_eq!(
            executor.apply_private_event(&mut live, &fill).unwrap(),
            PrivateEventOutcome::Applied
        );
        assert_eq!(live.state(), OrderState::PartiallyFilled);
        assert_eq!(
            executor.apply_private_event(&mut live, &fill).unwrap(),
            PrivateEventOutcome::Duplicate
        );

        executor.cancel(&mut live).unwrap();
        assert_eq!(live.state(), OrderState::CancelPending);
        executor.reconcile(&mut live).unwrap();
        assert_eq!(live.state(), OrderState::Cancelled);
        assert_eq!(live.cumulative_filled(), dec("0.4"));
        assert_eq!(calls.get(), 2);

        let (transport, journal) = executor.into_parts();
        assert_eq!(transport.exchange_requests[0]["action"]["type"], "order");
        assert_eq!(
            transport.exchange_requests[1]["action"]["type"],
            "cancelByCloid"
        );
        assert_eq!(transport.info_requests[0]["type"], "orderStatus");
        let recovered = LiveOrder::recover(
            live.order().clone(),
            live.client_order_id(),
            &journal.events,
        )
        .unwrap();
        assert_eq!(recovered.state(), OrderState::Cancelled);
        assert!(recovered.seen_private_events.contains("fill:7"));
    }

    #[test]
    fn uncertain_submission_reconciles_before_returning() {
        let calls = Rc::new(Cell::new(0));
        let mut transport = ScriptedTransport::default();
        transport.exchange.push_back(Err(TransportFailure {
            classification: SubmissionFailure::UncertainAfterWrite,
            operation: "exchange",
            status: None,
        }));
        transport
            .info
            .push_back(Ok(order_status("open", 77, "1", "1", 10_003)));
        let mut executor = executor(transport, calls);

        let live = executor.place(&intent(), &Resolver, &Allow).unwrap();

        assert_eq!(live.state(), OrderState::Open);
        assert_eq!(live.venue_order_id(), Some(77));
        let (_, journal) = executor.into_parts();
        assert!(journal.events.iter().any(|event| matches!(
            event.event,
            JournalEventKind::LifecycleTransition { ref transition }
                if transition.to == OrderState::SubmissionUncertain
        )));
    }

    #[test]
    fn kill_switch_engaged_after_evaluate_aborts_before_the_irreversible_submit() {
        let calls = Rc::new(Cell::new(0));
        let transport = ScriptedTransport::default();
        let mut executor = executor(transport, Rc::clone(&calls));

        let live = executor
            .place(&intent(), &Resolver, &AllowThenKillSwitch)
            .unwrap();

        assert_eq!(live.state(), OrderState::Rejected);
        assert_eq!(
            live.rejection
                .as_ref()
                .map(|rejection| rejection.code.as_str()),
            Some("kill_switch")
        );
        // The signer was invoked (signing happens before the recheck) but the
        // order never reached transport.exchange: ScriptedTransport's empty
        // queue would have panicked on a submit attempt.
        assert_eq!(calls.get(), 1);
        let (transport, journal) = executor.into_parts();
        assert!(transport.exchange_requests.is_empty());
        assert!(journal.events.iter().any(|event| matches!(
            &event.event,
            JournalEventKind::RiskRejected { rejection } if rejection.code == "kill_switch"
        )));
    }

    #[test]
    fn current_nested_order_status_shape_reconciles_terminal_cancel() {
        let response = json!({
            "status": "order",
            "order": {
                "order": {
                    "coin": "BTC",
                    "side": "B",
                    "limitPx": "78812.0",
                    "sz": "0.00015",
                    "origSz": "0.00015",
                    "oid": 59_070_902_058_u64,
                    "timestamp": 1_788_278_273_581_i64,
                    "cloid": "0x914295bf0c3f947da027089acfb4c210"
                },
                "status": "canceled",
                "statusTimestamp": 1_788_278_277_416_i64
            }
        });

        let status = parse_order_status(&response, dec("0.00015"))
            .unwrap()
            .unwrap();
        assert_eq!(status.state, OrderState::Cancelled);
        assert_eq!(status.venue_order_id, Some(59_070_902_058));
        assert_eq!(status.cumulative_filled, Decimal::ZERO);
        assert_eq!(status.occurred_at_ms, 1_788_278_277_416);
    }

    #[test]
    fn fill_wins_cancel_race_without_regressing_to_cancelled() {
        let calls = Rc::new(Cell::new(0));
        let mut transport = ScriptedTransport::default();
        transport.exchange.push_back(Ok(resting_response(42)));
        transport.exchange.push_back(Ok(cancel_response()));
        let mut executor = executor(transport, calls);
        let mut live = executor.place(&intent(), &Resolver, &Allow).unwrap();
        executor.cancel(&mut live).unwrap();

        let fill = PrivateEvent::Fill {
            event_id: "fill:race".to_owned(),
            venue_order_id: 42,
            quantity: dec("1"),
            occurred_at_ms: 10_004,
        };
        executor.apply_private_event(&mut live, &fill).unwrap();

        assert_eq!(live.state(), OrderState::Filled);
    }

    #[test]
    fn venue_rejection_is_structured_and_durable() {
        let calls = Rc::new(Cell::new(0));
        let mut transport = ScriptedTransport::default();
        transport.exchange.push_back(Ok(json!({
            "status": "ok",
            "response": {"data": {"statuses": [{"error": "Insufficient margin"}]}}
        })));
        let mut executor = executor(transport, calls);
        let live = executor.place(&intent(), &Resolver, &Allow).unwrap();
        assert_eq!(live.state(), OrderState::Rejected);
        assert_eq!(live.report().rejection.unwrap().code, "order_rejected");
        let (_, journal) = executor.into_parts();
        let recovered = LiveOrder::recover(
            live.order().clone(),
            live.client_order_id(),
            &journal.events,
        )
        .unwrap();
        assert_eq!(
            recovered.report().rejection.unwrap().reason,
            "Insufficient margin"
        );
    }

    #[test]
    fn risk_rejection_never_invokes_signer_or_transport() {
        let calls = Rc::new(Cell::new(0));
        let transport = ScriptedTransport::default();
        let mut executor = executor(transport, Rc::clone(&calls));

        let live = executor.place(&intent(), &Resolver, &Reject).unwrap();

        assert_eq!(live.state(), OrderState::Rejected);
        assert_eq!(calls.get(), 0);
        let (transport, _) = executor.into_parts();
        assert!(transport.exchange_requests.is_empty());
    }

    #[test]
    fn malformed_private_fill_and_wrong_acknowledgement_fail_closed() {
        let malformed = json!({
            "channel": "userFills",
            "data": {"fills": [{"tid": 1, "oid": 2, "sz": "0", "time": 3}]}
        });
        assert!(parse_private_message(&malformed).is_err());
        assert!(TestnetAcknowledgement::new("yes").is_err());
    }

    #[test]
    fn executor_rejects_an_unpinned_signer_address() {
        let acknowledgement = TestnetAcknowledgement::new(TESTNET_ACKNOWLEDGEMENT).unwrap();
        let config = HyperliquidTestnetConfig::new(account(), 10_000, acknowledgement).unwrap();
        let result = HyperliquidTestnetExecutor::new(
            config,
            FixtureSigner {
                calls: Rc::new(Cell::new(0)),
                address: "0x2222222222222222222222222222222222222222",
            },
            ScriptedTransport::default(),
            InMemoryJournal::default(),
            SequenceClock(Cell::new(10_000)),
            Assets,
            TestnetReliability::new(
                RequestThrottle::new(100, 1_000).unwrap(),
                RetryPolicy::new(3, 100, 1_000).unwrap(),
            ),
        );

        assert!(result.is_err());
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("configured authorized signer")
        );

        let acknowledgement = TestnetAcknowledgement::new(TESTNET_ACKNOWLEDGEMENT).unwrap();
        let agent_address = "0x2222222222222222222222222222222222222222";
        let config = HyperliquidTestnetConfig::new(account(), 10_000, acknowledgement)
            .unwrap()
            .with_authorized_signer_address(agent_address)
            .unwrap();
        let result = HyperliquidTestnetExecutor::new(
            config,
            FixtureSigner {
                calls: Rc::new(Cell::new(0)),
                address: agent_address,
            },
            ScriptedTransport::default(),
            InMemoryJournal::default(),
            SequenceClock(Cell::new(10_000)),
            Assets,
            TestnetReliability::new(
                RequestThrottle::new(100, 1_000).unwrap(),
                RetryPolicy::new(3, 100, 1_000).unwrap(),
            ),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn signed_request_rejects_provider_mutation_and_vault_injection() {
        let expected = HyperliquidSigningRequest {
            network: ExecutionNetwork::Testnet,
            nonce: 10_000,
            expires_after: 20_000,
            action: json!({"type": "cancelByCloid", "cancels": []}),
        };
        let signature = json!({
            "r": format!("0x{}", "01".repeat(32)),
            "s": format!("0x{}", "02".repeat(32)),
            "v": 27
        });
        let changed_nonce = json!({
            "action": expected.action(),
            "nonce": 10_001,
            "signature": signature,
            "expiresAfter": expected.expires_after()
        });
        assert!(validate_signed_request(&changed_nonce, &expected).is_err());

        let vault = json!({
            "action": expected.action(),
            "nonce": expected.nonce(),
            "signature": changed_nonce["signature"].clone(),
            "vaultAddress": account(),
            "expiresAfter": expected.expires_after()
        });
        assert!(validate_signed_request(&vault, &expected).is_err());
    }

    #[cfg(feature = "hypersdk-signer")]
    #[test]
    fn hypersdk_signer_produces_recoverable_order_and_cancel_requests() {
        use std::str::FromStr as _;

        let local = hypersdk::hypercore::PrivateKeySigner::from_str(
            "e908f86dbb4d55ac876378565aafeabc187f6690f046459397b17d9b9a19688e",
        )
        .unwrap();
        let signer = HypersdkTestnetSigner::new(local, "fixture/hypersdk-testnet").unwrap();
        let actions = [
            json!({
                "type": "order",
                "orders": [{
                    "a": 0,
                    "b": true,
                    "p": "100",
                    "s": "1",
                    "r": false,
                    "t": {"limit": {"tif": "Gtc"}},
                    "c": "0x11111111111111111111111111111111"
                }],
                "grouping": "na"
            }),
            json!({
                "type": "cancelByCloid",
                "cancels": [{
                    "asset": 0,
                    "cloid": "0x11111111111111111111111111111111"
                }]
            }),
        ];

        for (offset, action) in actions.into_iter().enumerate() {
            let request = HyperliquidSigningRequest {
                network: ExecutionNetwork::Testnet,
                nonce: 10_000 + u64::try_from(offset).unwrap(),
                expires_after: 20_000 + u64::try_from(offset).unwrap(),
                action,
            };
            let signed_request = signer.sign_l1_action(&request).unwrap();
            validate_signed_request(&signed_request, &request).unwrap();
            let sdk_request: hypersdk::hypercore::ActionRequest =
                serde_json::from_value(signed_request).unwrap();
            assert_eq!(
                sdk_request
                    .recover(hypersdk::hypercore::Chain::Testnet)
                    .unwrap()
                    .to_string()
                    .to_ascii_lowercase(),
                signer.signer_address()
            );
        }
    }

    #[test]
    fn private_subscriptions_are_account_scoped() {
        let subscriptions = private_subscription_requests(account()).unwrap();
        assert_eq!(subscriptions[0]["subscription"]["type"], "orderUpdates");
        assert_eq!(subscriptions[1]["subscription"]["type"], "userFills");
        assert_eq!(subscriptions[0]["subscription"]["user"], account());
    }

    fn executor(
        transport: ScriptedTransport,
        calls: Rc<Cell<usize>>,
    ) -> HyperliquidTestnetExecutor<
        FixtureSigner,
        ScriptedTransport,
        InMemoryJournal,
        SequenceClock,
        Assets,
    > {
        let acknowledgement = TestnetAcknowledgement::new(TESTNET_ACKNOWLEDGEMENT).unwrap();
        let config = HyperliquidTestnetConfig::new(account(), 10_000, acknowledgement).unwrap();
        HyperliquidTestnetExecutor::new(
            config,
            FixtureSigner {
                calls,
                address: account(),
            },
            transport,
            InMemoryJournal::default(),
            SequenceClock(Cell::new(10_000)),
            Assets,
            TestnetReliability::new(
                RequestThrottle::new(100, 1_000).unwrap(),
                RetryPolicy::new(3, 100, 1_000).unwrap(),
            ),
        )
        .unwrap()
    }

    fn intent() -> OrderIntent {
        OrderIntent::limit(
            "testnet-lifecycle-1",
            "hyperliquid",
            "BTC",
            Side::Buy,
            dec("1"),
            dec("100"),
            1_000,
        )
        .unwrap()
    }

    fn account() -> &'static str {
        "0x1111111111111111111111111111111111111111"
    }

    fn resting_response(oid: u64) -> Value {
        json!({
            "status": "ok",
            "response": {"type": "order", "data": {"statuses": [{"resting": {"oid": oid}}]}}
        })
    }

    fn cancel_response() -> Value {
        json!({
            "status": "ok",
            "response": {"type": "cancel", "data": {"statuses": ["success"]}}
        })
    }

    fn order_status(
        status: &str,
        oid: u64,
        original: &str,
        remaining: &str,
        timestamp: i64,
    ) -> Value {
        json!({
            "order": {
                "oid": oid,
                "origSz": original,
                "sz": remaining,
                "timestamp": timestamp
            },
            "status": status,
            "statusTimestamp": timestamp
        })
    }

    fn dec(value: &str) -> Decimal {
        value.parse().unwrap()
    }
}
