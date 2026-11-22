//! Durable bounded stop-failure recovery. This module has no signing authority.
//! A host must supply a scoped venue adapter, an account-wide entry gate and a
//! single-writer journal. Native exits remain live until flatness is reconciled.
#![allow(missing_docs)]
use crate::{
    ExecutionError, ExecutionNetwork, Journal, JournalEvent, JournalEventKind, Side,
    bracket::{BracketManager, BracketSpec, EntryOutcome, ExitObservation},
};
use hypercarry_notifications::{Kind, NotificationEvent};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, ExecutionError>;
fn check(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(ExecutionError::Validation(message.into()))
    }
}
fn arithmetic(value: Option<Decimal>) -> Result<Decimal> {
    value.ok_or_else(|| ExecutionError::Validation("recovery decimal overflow".into()))
}
fn digest(value: &impl Serialize) -> Result<String> {
    hypercarry_notifications::digest(value)
        .map_err(|_| ExecutionError::Validation("recovery identity encoding failed".into()))
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryLimits {
    #[serde(with = "rust_decimal::serde::str")]
    pub size_step: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub price_tick: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub min_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub max_price: Decimal,
    pub slippage_bps: u32,
    pub stop_grace_ms: u64,
    pub max_snapshot_age_ms: u64,
    pub max_close_attempts: u32,
    pub max_cancel_attempts: u32,
    pub retry_ms: u64,
    pub recovery_timeout_ms: u64,
    pub action_ttl_ms: u64,
}
impl RecoveryLimits {
    /// # Errors
    /// Rejects invalid scope, precision or unbounded policy.
    pub fn validate(&self) -> Result<()> {
        check(
            self.size_step > Decimal::ZERO
                && self.price_tick > Decimal::ZERO
                && self.min_price > Decimal::ZERO
                && self.max_price >= self.min_price
                && self.min_price.checked_rem(self.price_tick) == Some(Decimal::ZERO)
                && self.max_price.checked_rem(self.price_tick) == Some(Decimal::ZERO),
            "invalid recovery precision/price bounds",
        )?;
        check(
            (1..=100).contains(&self.slippage_bps)
                && self.stop_grace_ms <= 30_000
                && (100..=10_000).contains(&self.max_snapshot_age_ms)
                && (1..=8).contains(&self.max_close_attempts)
                && (1..=8).contains(&self.max_cancel_attempts)
                && (1000..=30_000).contains(&self.retry_ms)
                && (5000..=300_000).contains(&self.recovery_timeout_ms)
                && self.stop_grace_ms < self.recovery_timeout_ms
                && (1000..=10_000).contains(&self.action_ttl_ms),
            "invalid bounded recovery policy",
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPolicy {
    pub spec: BracketSpec,
    pub limits: RecoveryLimits,
    /// Replayed repair identities; absent in legacy journals. Never changes `spec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repaired_exits: Option<RecoveryExitIds>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryExitIds {
    pub tp: String,
    pub sl: String,
}
impl RecoveryPolicy {
    /// Bind the replayed exit identities without changing the approved specification.
    /// # Errors
    /// Rejects unreserved brackets, invalid exit identities or unbounded limits.
    pub fn from_bracket(bracket: &BracketManager, limits: RecoveryLimits) -> Result<Self> {
        check(
            bracket.entry_was_reserved(),
            "recovery requires an owned bracket entry",
        )?;
        let (tp, sl) = bracket.exit_cloids();
        let policy = Self {
            spec: bracket.spec.clone(),
            limits,
            repaired_exits: (tp != bracket.spec.tp_cloid || sl != bracket.spec.sl_cloid).then(
                || RecoveryExitIds {
                    tp: tp.into(),
                    sl: sl.into(),
                },
            ),
        };
        policy.validate()?;
        Ok(policy)
    }
    pub fn tp_cloid(&self) -> &str {
        self.repaired_exits
            .as_ref()
            .map_or(&self.spec.tp_cloid, |ids| &ids.tp)
    }
    pub fn sl_cloid(&self) -> &str {
        self.repaired_exits
            .as_ref()
            .map_or(&self.spec.sl_cloid, |ids| &ids.sl)
    }
    /// # Errors
    /// Rejects invalid scope, precision or unbounded policy.
    pub fn validate(&self) -> Result<()> {
        self.spec.validate()?;
        self.limits.validate()?;
        // Reuse the complete ID validator, without mutating the original spec.
        let mut effective = self.spec.clone();
        effective.tp_cloid = self.tp_cloid().into();
        effective.sl_cloid = self.sl_cloid().into();
        effective.validate()?;
        check(
            self.tp_cloid() != self.spec.sl_cloid && self.sl_cloid() != self.spec.tp_cloid,
            "repair cannot exchange original exit identities",
        )?;
        check(
            self.spec.coin.len() <= 32
                && self.spec.quantity.checked_rem(self.limits.size_step) == Some(Decimal::ZERO),
            "unquantized recovery size",
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedOrderState {
    Unknown,
    Open,
    Terminal,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderEvidence {
    pub cloid: String,
    pub state: ObservedOrderState,
}
/// Fresh REST evidence. Stream events can wake the loop but cannot replace this.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySnapshot {
    pub network: ExecutionNetwork,
    pub account: String,
    pub coin: String,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
    pub complete: bool,
    pub agent_authorized: bool,
    pub entry: EntryOutcome,
    #[serde(with = "rust_decimal::serde::str")]
    pub signed_position: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub mark: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub bid: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub ask: Decimal,
    pub tp: Option<ExitObservation>,
    pub sl: Option<ExitObservation>,
    /// Derived from the owned SL's authoritative status, not an unrelated fill.
    pub stop_fired: bool,
    pub unmanaged_orders: u32,
    pub pending: Option<OrderEvidence>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryAction {
    CancelOwned {
        cloid: String,
    },
    /// The adapter MUST submit this as reduce-only IOC, never as an entry/GTC.
    CloseRemaining {
        cloid: String,
        side: Side,
        #[serde(with = "rust_decimal::serde::str")]
        quantity: Decimal,
        #[serde(with = "rust_decimal::serde::str")]
        limit_price: Decimal,
    },
}
impl RecoveryAction {
    pub fn cloid(&self) -> &str {
        match self {
            Self::CancelOwned { cloid } | Self::CloseRemaining { cloid, .. } => cloid,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservedAction {
    pub action: RecoveryAction,
    pub nonce: u64,
    pub expires_at_ms: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryPhase {
    Monitoring,
    Grace,
    Reconciling,
    Recovering,
    Escalated,
    Closed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Healthy,
    StopResidual,
    ProtectionMissing,
    StateUnknown,
    UnexpectedExposure,
    AgentRevoked,
    PriceOutsideBound,
    Dust,
    Deadline,
    AttemptsExhausted,
    PendingEntry,
    CloseReserved,
    CancelReserved,
    Flat,
    SubmissionUncertain,
    SubmissionRejected,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryState {
    pub phase: RecoveryPhase,
    pub reason: Reason,
    pub fault_since_ms: Option<u64>,
    pub last_time_ms: u64,
    pub last_snapshot_ms: u64,
    pub last_nonce: u64,
    pub next_action_ms: u64,
    pub entry_terminal_seen: bool,
    pub close_attempts: u32,
    pub cancel_attempts: u32,
    #[serde(with = "hypercarry_notifications::optional_decimal")]
    pub remaining: Option<Decimal>,
    pub pending: Option<ReservedAction>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRecord {
    pub policy: RecoveryPolicy,
    pub state: RecoveryState,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    Accepted,
    Rejected,
    Unknown,
}
/// Host boundary: freeze must durably close the entry gate shared with ALL entry
/// processes for this account. All I/O must have deadlines; unknown writes must
/// never be retried inside the adapter. The host owns the account/agent lock.
pub trait RecoveryVenue {
    /// # Errors
    /// Fails if the shared entry gate cannot be durably closed.
    fn freeze_entries(&mut self) -> Result<()>;
    /// # Errors
    /// Fails if trustworthy wall time is unavailable.
    fn now_ms(&self) -> Result<u64>;
    /// # Errors
    /// Fails on missing, inconsistent or malformed venue evidence.
    fn snapshot(
        &mut self,
        policy: &RecoveryPolicy,
        pending: Option<&ReservedAction>,
    ) -> Result<RecoverySnapshot>;
    fn submit(&mut self, policy: &RecoveryPolicy, action: &ReservedAction) -> SubmitOutcome;
}
#[derive(Debug, Clone)]
pub struct RecoveryManager {
    policy: RecoveryPolicy,
    state: RecoveryState,
    next_sequence: u64,
}
impl RecoveryManager {
    /// Start only from a bracket whose entry was durably reserved. This is not
    /// permission to adopt a manual position or to bypass mainnet release gates.
    /// # Errors
    /// Rejects unowned entries, invalid policy or journal failures.
    pub fn from_bracket<J: Journal>(
        bracket: &BracketManager,
        limits: RecoveryLimits,
        journal: &mut J,
        now: u64,
    ) -> Result<Self> {
        check(
            bracket.entry_was_reserved(),
            "recovery requires an owned bracket entry",
        )?;
        let policy = RecoveryPolicy::from_bracket(bracket, limits)?;
        check(
            journal.next_sequence() == Some(0) && now > 0,
            "new dedicated recovery journal required",
        )?;
        let mut manager = Self {
            policy,
            state: RecoveryState {
                phase: RecoveryPhase::Reconciling,
                reason: Reason::StateUnknown,
                fault_since_ms: None,
                last_time_ms: now,
                last_snapshot_ms: 0,
                last_nonce: bracket.entry_nonce().unwrap_or(0),
                next_action_ms: now,
                entry_terminal_seen: false,
                close_attempts: 0,
                cancel_attempts: 0,
                remaining: None,
                pending: None,
            },
            next_sequence: 0,
        };
        manager.persist(journal, manager.state.clone(), now)?;
        Ok(manager)
    }
    pub fn policy(&self) -> &RecoveryPolicy {
        &self.policy
    }
    pub fn state(&self) -> &RecoveryState {
        &self.state
    }
    /// Never reopens entry authority automatically, even after flatness.
    pub const fn entries_frozen(&self) -> bool {
        true
    }
    /// # Errors
    /// Rejects mixed scope, regressed history or invalid budgets.
    pub fn replay(records: &[JournalEvent]) -> Result<Self> {
        let mut manager: Option<Self> = None;
        for (i, r) in records.iter().enumerate() {
            let JournalEventKind::Recovery {
                record,
                notification: _,
            } = &r.event
            else {
                return Err(ExecutionError::JournalSchema(
                    "mixed recovery journal".into(),
                ));
            };
            record.policy.validate()?;
            check(
                r.sequence == i as u64
                    && r.schema_version == 1
                    && r.correlation_id == record.policy.spec.trade_id
                    && r.recorded_at_ms == i64::try_from(record.state.last_time_ms).unwrap_or(-1),
                "invalid recovery record identity/time",
            )?;
            if let Some(m) = &manager {
                check(
                    m.policy == record.policy
                        && record.state.last_time_ms >= m.state.last_time_ms
                        && record.state.last_nonce >= m.state.last_nonce
                        && record.state.close_attempts >= m.state.close_attempts
                        && record.state.cancel_attempts >= m.state.cancel_attempts,
                    "recovery history regressed or changed scope",
                )?;
            }
            check(
                record.state.close_attempts <= record.policy.limits.max_close_attempts
                    && record.state.cancel_attempts <= record.policy.limits.max_cancel_attempts,
                "recovery budget exceeded",
            )?;
            manager = Some(Self {
                policy: record.policy.clone(),
                state: record.state.clone(),
                next_sequence: r.sequence + 1,
            });
        }
        manager.ok_or_else(|| ExecutionError::JournalSchema("empty recovery journal".into()))
    }
    fn persist<J: Journal>(
        &mut self,
        journal: &mut J,
        mut next: RecoveryState,
        now: u64,
    ) -> Result<()> {
        check(
            now >= self.state.last_time_ms
                && now > 0
                && journal.next_sequence() == Some(self.next_sequence),
            "recovery clock/journal mismatch",
        )?;
        next.last_time_ms = now;
        // Read-only polling need not append identical durable decisions every
        // second. Keep freshness in memory; restart always obtains fresh REST.
        let mut comparable = next.clone();
        comparable.last_time_ms = self.state.last_time_ms;
        comparable.last_snapshot_ms = self.state.last_snapshot_ms;
        if self.next_sequence > 0 && comparable == self.state {
            self.state = next;
            return Ok(());
        }
        let kind =
            if next.phase == RecoveryPhase::Closed && self.state.phase != RecoveryPhase::Closed {
                Some(Kind::PositionClosed)
            } else if self.next_sequence == 0
                || next.reason != self.state.reason
                || next.phase != self.state.phase
            {
                match next.reason {
                    Reason::Healthy => Some(Kind::ProtectionVerified),
                    Reason::StopResidual => Some(Kind::StopResidual),
                    Reason::ProtectionMissing => Some(Kind::ProtectionMissing),
                    Reason::AgentRevoked => Some(Kind::AgentRevoked),
                    Reason::StateUnknown | Reason::SubmissionUncertain => Some(Kind::StateUnknown),
                    Reason::PriceOutsideBound
                    | Reason::Dust
                    | Reason::Deadline
                    | Reason::AttemptsExhausted
                    | Reason::UnexpectedExposure => Some(Kind::RecoveryFailed),
                    _ => None,
                }
            } else {
                None
            };
        let notification = kind
            .map(|kind| -> Result<_> {
                Ok(NotificationEvent {
                    schema_version: 1,
                    id: digest(&(
                        "recovery-notification-v1",
                        &self.policy,
                        self.next_sequence,
                        kind,
                    ))?,
                    network: self.policy.spec.network.to_string(),
                    trade_id: self.policy.spec.trade_id.clone(),
                    coin: self.policy.spec.coin.clone(),
                    kind,
                    observed_at_ms: now,
                    remaining: if kind == Kind::StateUnknown {
                        None
                    } else {
                        next.remaining
                    },
                })
            })
            .transpose()?;
        journal.append(
            i64::try_from(now)
                .map_err(|_| ExecutionError::Validation("timestamp overflow".into()))?,
            &self.policy.spec.trade_id,
            JournalEventKind::Recovery {
                record: Box::new(RecoveryRecord {
                    policy: self.policy.clone(),
                    state: next.clone(),
                }),
                notification: notification.map(Box::new),
            },
        )?;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| ExecutionError::JournalSchema("sequence overflow".into()))?;
        self.state = next;
        Ok(())
    }
    fn alert<J: Journal>(
        &mut self,
        journal: &mut J,
        now: u64,
        reason: Reason,
        terminal: bool,
    ) -> Result<()> {
        let mut next = self.state.clone();
        next.phase = if terminal
            || matches!(
                self.state.phase,
                RecoveryPhase::Escalated | RecoveryPhase::Closed
            ) {
            RecoveryPhase::Escalated
        } else {
            RecoveryPhase::Reconciling
        };
        next.reason = reason;
        next.fault_since_ms.get_or_insert(now);
        if next.fault_since_ms.is_some_and(|since| {
            now.saturating_sub(since) >= self.policy.limits.recovery_timeout_ms
        }) {
            next.phase = RecoveryPhase::Escalated;
            next.reason = Reason::Deadline;
        }
        self.persist(journal, next, now)
    }
    /// One bounded pass: freeze, read fresh evidence, reserve durably, submit at
    /// most ONE action. Acknowledgement never establishes a fill or flatness.
    /// # Errors
    /// Fails on entry-gate, clock, policy or durability errors before new writes.
    pub fn tick<J: Journal>(
        &mut self,
        journal: &mut J,
        venue: &mut impl RecoveryVenue,
    ) -> Result<()> {
        venue.freeze_entries()?;
        let now = venue.now_ms()?;
        check(now >= self.state.last_time_ms, "recovery clock regressed")?;
        let Ok(snapshot) = venue.snapshot(&self.policy, self.state.pending.as_ref()) else {
            let completed = venue.now_ms()?;
            check(completed >= now, "recovery clock regressed during read")?;
            self.alert(journal, completed, Reason::StateUnknown, false)?;
            return Ok(());
        };
        let now = venue.now_ms()?;
        let action = self.observe(journal, &snapshot, now)?;
        if let Some(reserved) = action {
            // Reads or durable writes may consume time; never submit an expired reservation.
            let send_time = venue.now_ms()?;
            if send_time >= reserved.expires_at_ms || send_time < now {
                return self.alert(
                    journal,
                    send_time.max(now),
                    Reason::SubmissionUncertain,
                    false,
                );
            }
            let outcome = venue.submit(&self.policy, &reserved);
            let mut next = self.state.clone();
            next.reason = match outcome {
                SubmitOutcome::Accepted => next.reason,
                SubmitOutcome::Rejected => Reason::SubmissionRejected,
                SubmitOutcome::Unknown => Reason::SubmissionUncertain,
            };
            // Even a rejection needs fresh position/entry reconciliation before retry.
            // A definitive rejection permits another bounded attempt, but only
            // after a new snapshot. Unknown writes retain their reservation.
            if outcome == SubmitOutcome::Rejected {
                next.pending = None;
            }
            self.persist(journal, next, venue.now_ms()?)?;
        }
        Ok(())
    }
    #[allow(clippy::too_many_lines)]
    fn observe<J: Journal>(
        &mut self,
        journal: &mut J,
        snapshot: &RecoverySnapshot,
        now: u64,
    ) -> Result<Option<ReservedAction>> {
        let policy = self.policy.clone();
        let spec = &policy.spec;
        let limits = &policy.limits;
        if snapshot.account != spec.account
            || snapshot.network != spec.network
            || snapshot.coin != spec.coin
            || !snapshot.complete
            || snapshot.started_at_ms < self.state.last_snapshot_ms
            || snapshot.completed_at_ms < snapshot.started_at_ms
            || snapshot.completed_at_ms > now
            || now.saturating_sub(snapshot.started_at_ms) > limits.max_snapshot_age_ms
        {
            self.alert(journal, now, Reason::StateUnknown, false)?;
            return Ok(None);
        }
        let mut next = self.state.clone();
        next.last_snapshot_ms = snapshot.completed_at_ms;
        next.remaining = Some(snapshot.signed_position.abs());
        let quantity = snapshot.signed_position.abs();
        let direction_ok = if spec.entry_side == Side::Sell {
            snapshot.signed_position <= Decimal::ZERO
        } else {
            snapshot.signed_position >= Decimal::ZERO
        };
        if !direction_ok
            || quantity > spec.quantity
            || (self.state.entry_terminal_seen
                && self.state.remaining.is_some_and(|old| quantity > old))
            || snapshot.unmanaged_orders > 0
            || snapshot
                .tp
                .as_ref()
                .is_some_and(|v| v.cloid != policy.tp_cloid() || v.coin != spec.coin)
            || snapshot
                .sl
                .as_ref()
                .is_some_and(|v| v.cloid != policy.sl_cloid() || v.coin != spec.coin)
        {
            next.phase = RecoveryPhase::Escalated;
            next.reason = Reason::UnexpectedExposure;
            self.persist(journal, next, now)?;
            return Ok(None);
        }
        next.entry_terminal_seen |= snapshot.entry == EntryOutcome::Terminal;
        if let Some(pending) = &next.pending {
            let resolved = match &pending.action {
                RecoveryAction::CloseRemaining { cloid, .. } => snapshot
                    .pending
                    .as_ref()
                    .is_some_and(|e| e.cloid == *cloid && e.state == ObservedOrderState::Terminal),
                RecoveryAction::CancelOwned { cloid } if *cloid == spec.entry_cloid => {
                    snapshot.entry == EntryOutcome::Terminal
                }
                RecoveryAction::CancelOwned { cloid } if cloid == policy.tp_cloid() => {
                    snapshot.tp.is_none()
                }
                RecoveryAction::CancelOwned { cloid } if cloid == policy.sl_cloid() => {
                    snapshot.sl.is_none()
                }
                RecoveryAction::CancelOwned { .. } => false,
            };
            if resolved {
                next.pending = None;
            }
        }
        // A flat snapshot cannot resolve an unknown IOC: it could arrive late
        // and reduce a later position. Keep the gate closed and reconcile it.
        if quantity.is_zero()
            && snapshot.entry == EntryOutcome::Terminal
            && snapshot.tp.is_none()
            && snapshot.sl.is_none()
            && next.pending.is_none()
        {
            next.phase = RecoveryPhase::Closed;
            next.reason = Reason::Flat;
            self.persist(journal, next, now)?;
            return Ok(None);
        }
        // Escalation is sticky; later good snapshots cannot reset budgets/authority.
        if self.state.phase == RecoveryPhase::Escalated || self.state.phase == RecoveryPhase::Closed
        {
            next.phase = RecoveryPhase::Escalated;
            if self.state.phase == RecoveryPhase::Closed {
                next.reason = Reason::UnexpectedExposure;
            }
            self.persist(journal, next, now)?;
            return Ok(None);
        }
        if !snapshot.agent_authorized {
            next.phase = RecoveryPhase::Escalated;
            next.reason = Reason::AgentRevoked;
            self.persist(journal, next, now)?;
            return Ok(None);
        }
        let stop_crossed = snapshot.stop_fired
            || (if spec.entry_side == Side::Sell {
                snapshot.mark >= spec.sl
            } else {
                snapshot.mark <= spec.sl
            });
        let exit_valid = |v: Option<&ExitObservation>, tp: bool| {
            v.is_some_and(|v| {
                v.side != spec.entry_side
                    && v.reduce_only
                    && v.is_trigger
                    && v.is_take_profit == tp
                    && v.quantity >= quantity
                    && v.trigger == if tp { spec.tp } else { spec.sl }
            })
        };
        let protected =
            exit_valid(snapshot.tp.as_ref(), true) && exit_valid(snapshot.sl.as_ref(), false);
        if !quantity.is_zero() && (stop_crossed || !protected) {
            next.fault_since_ms.get_or_insert(now);
            next.reason = if stop_crossed {
                Reason::StopResidual
            } else {
                Reason::ProtectionMissing
            };
        }
        if self.state.fault_since_ms.is_none() && next.fault_since_ms.is_some() {
            self.persist(journal, next.clone(), now)?;
        }
        if snapshot.entry != EntryOutcome::Terminal || quantity.is_zero() {
            next.fault_since_ms.get_or_insert(now);
        }
        if next
            .fault_since_ms
            .is_some_and(|t| now.saturating_sub(t) >= limits.recovery_timeout_ms)
        {
            next.phase = RecoveryPhase::Escalated;
            next.reason = Reason::Deadline;
            self.persist(journal, next, now)?;
            return Ok(None);
        }
        if next.pending.is_some() || snapshot.entry == EntryOutcome::Unknown {
            next.phase = RecoveryPhase::Reconciling;
            next.reason = Reason::StateUnknown;
            self.persist(journal, next, now)?;
            return Ok(None);
        }
        if now < next.next_action_ms {
            self.persist(journal, next, now)?;
            return Ok(None);
        }
        let action = if snapshot.entry == EntryOutcome::Pending {
            next.reason = Reason::PendingEntry;
            Some(RecoveryAction::CancelOwned {
                cloid: spec.entry_cloid.clone(),
            })
        } else if quantity.is_zero() {
            snapshot
                .tp
                .as_ref()
                .or(snapshot.sl.as_ref())
                .map(|v| RecoveryAction::CancelOwned {
                    cloid: v.cloid.clone(),
                })
        } else if let Some(since) = next.fault_since_ms {
            if now.saturating_sub(since) < limits.stop_grace_ms {
                next.phase = RecoveryPhase::Grace;
                self.persist(journal, next, now)?;
                return Ok(None);
            }
            if snapshot.bid <= Decimal::ZERO
                || snapshot.ask < snapshot.bid
                || snapshot.mark <= Decimal::ZERO
            {
                next.phase = RecoveryPhase::Reconciling;
                next.reason = Reason::StateUnknown;
                self.persist(journal, next, now)?;
                return Ok(None);
            }
            let buy = spec.entry_side == Side::Sell;
            let price = if buy { snapshot.ask } else { snapshot.bid };
            if price < limits.min_price || price > limits.max_price {
                next.phase = RecoveryPhase::Escalated;
                next.reason = Reason::PriceOutsideBound;
                self.persist(journal, next, now)?;
                return Ok(None);
            }
            if quantity.checked_rem(limits.size_step) != Some(Decimal::ZERO) {
                next.phase = RecoveryPhase::Escalated;
                next.reason = Reason::Dust;
                self.persist(journal, next, now)?;
                return Ok(None);
            }
            let slip =
                arithmetic(Decimal::from(limits.slippage_bps).checked_div(Decimal::from(10_000)))?;
            let factor = if buy {
                arithmetic(Decimal::ONE.checked_add(slip))?
            } else {
                arithmetic(Decimal::ONE.checked_sub(slip))?
            };
            let raw = arithmetic(price.checked_mul(factor))?;
            let ticks = arithmetic(raw.checked_div(limits.price_tick))?;
            let rounded = arithmetic(
                (if buy { ticks.floor() } else { ticks.ceil() }).checked_mul(limits.price_tick),
            )?;
            let limit_price = rounded.max(limits.min_price).min(limits.max_price);
            if (buy && limit_price < snapshot.ask) || (!buy && limit_price > snapshot.bid) {
                // A coarse tick or absolute bound can leave no executable price
                // inside the approved collar. Do not burn retries on a resting
                // price (or silently widen the user's slippage allowance).
                next.phase = RecoveryPhase::Escalated;
                next.reason = Reason::PriceOutsideBound;
                self.persist(journal, next, now)?;
                return Ok(None);
            }
            let id = digest(&("recovery-close-v1", &policy, next.close_attempts + 1))?;
            Some(RecoveryAction::CloseRemaining {
                cloid: format!("0x{}", &id[..32]),
                side: if buy { Side::Buy } else { Side::Sell },
                quantity,
                limit_price,
            })
        } else {
            next.phase = RecoveryPhase::Monitoring;
            next.reason = Reason::Healthy;
            None
        };
        let Some(action) = action else {
            self.persist(journal, next, now)?;
            return Ok(None);
        };
        let close = matches!(action, RecoveryAction::CloseRemaining { .. });
        if (close && next.close_attempts >= limits.max_close_attempts)
            || (!close && next.cancel_attempts >= limits.max_cancel_attempts)
        {
            next.phase = RecoveryPhase::Escalated;
            next.reason = Reason::AttemptsExhausted;
            self.persist(journal, next, now)?;
            return Ok(None);
        }
        if close {
            next.close_attempts += 1;
            next.reason = Reason::CloseReserved;
        } else {
            next.cancel_attempts += 1;
            next.reason = Reason::CancelReserved;
        }
        let nonce = now.max(
            next.last_nonce
                .checked_add(1)
                .ok_or_else(|| ExecutionError::Validation("nonce overflow".into()))?,
        );
        let reserved = ReservedAction {
            action,
            nonce,
            expires_at_ms: now
                .checked_add(limits.action_ttl_ms)
                .ok_or_else(|| ExecutionError::Validation("expiry overflow".into()))?,
        };
        next.last_nonce = nonce;
        next.pending = Some(reserved.clone());
        next.phase = RecoveryPhase::Recovering;
        next.next_action_ms = now.saturating_add(limits.retry_ms);
        self.persist(journal, next, now)?;
        Ok(Some(reserved))
    }
}
