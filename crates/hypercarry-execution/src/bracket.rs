//! Restartable, journal-backed bracket reconciliation. No signer or transport.
//! This domain engine does not turn an approval into live mainnet authority.
#![allow(missing_docs)]
use crate::{ExecutionError, ExecutionNetwork, Journal, JournalEvent, JournalEventKind, Side};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt::Write as _};

type Result<T> = std::result::Result<T, ExecutionError>;
fn require(test: bool, message: &str) -> Result<()> {
    if test {
        Ok(())
    } else {
        Err(ExecutionError::Validation(message.into()))
    }
}
fn decimal(value: Option<Decimal>) -> Result<Decimal> {
    value.ok_or_else(|| ExecutionError::Validation("bracket arithmetic overflow".into()))
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BracketSpec {
    pub trade_id: String,
    /// Digest of the full reviewed plan, including entry bounds and fee assumptions.
    /// A lifecycle approval alone remains insufficient for live signing authority.
    pub plan_digest: String,
    pub network: ExecutionNetwork,
    pub account: String,
    pub coin: String,
    pub entry_side: Side,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub tp: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub sl: Decimal,
    pub entry_cloid: String,
    pub tp_cloid: String,
    pub sl_cloid: String,
}
impl BracketSpec {
    /// Validate immutable trade identity and exit directions.
    ///
    /// # Errors
    /// Rejects invalid identity, duplicate client IDs and nonpositive quantities/prices.
    pub fn validate(&self) -> Result<()> {
        require(
            self.plan_digest.len() == 64 && self.plan_digest.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid full-plan digest",
        )?;
        require(
            !self.trade_id.is_empty()
                && self.trade_id.len() <= 64
                && self
                    .trade_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "invalid trade id",
        )?;
        require(
            self.account.len() == 42
                && self.account.starts_with("0x")
                && self.account[2..].bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid account",
        )?;
        require(
            !self.coin.is_empty()
                && self.coin.len() <= 32
                && self.coin.split(':').count() <= 2
                && self.coin.split(':').all(|part| {
                    !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric())
                }),
            "invalid coin",
        )?;
        require(
            self.quantity > Decimal::ZERO && self.tp > Decimal::ZERO && self.sl > Decimal::ZERO,
            "invalid size or exits",
        )?;
        require(
            if self.entry_side == Side::Sell {
                self.tp < self.sl
            } else {
                self.tp > self.sl
            },
            "exit directions inverted",
        )?;
        let mut ids = std::collections::BTreeSet::new();
        for id in [&self.entry_cloid, &self.tp_cloid, &self.sl_cloid] {
            require(
                id.len() == 34
                    && id.starts_with("0x")
                    && id[2..].bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid client order id",
            )?;
            require(ids.insert(id.to_lowercase()), "duplicate client order ids")?;
        }
        Ok(())
    }
    /// Hash the validated immutable lifecycle specification.
    ///
    /// # Errors
    /// Returns validation or serialization failures.
    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        Ok(Sha256::digest(serde_json::to_vec(self)?).iter().fold(
            String::with_capacity(64),
            |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            },
        ))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BracketState {
    Draft,
    Approved,
    Configured,
    EntryPending,
    ReconciliationRequired,
    NeedsProtection,
    Open,
    ExitPending,
    Closed,
    Degraded,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryOutcome {
    Unknown,
    Pending,
    Terminal,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExitObservation {
    pub cloid: String,
    pub coin: String,
    pub side: Side,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub trigger: Decimal,
    pub reduce_only: bool,
    pub is_trigger: bool,
    pub is_take_profit: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BracketObservation {
    pub account: String,
    pub network: ExecutionNetwork,
    pub coin: String,
    pub entry_outcome: EntryOutcome,
    #[serde(with = "rust_decimal::serde::str")]
    pub signed_position: Decimal,
    pub tp: Option<ExitObservation>,
    pub sl: Option<ExitObservation>,
    pub unmanaged_orders: u32,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub complete: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillRole {
    Entry,
    Exit,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BracketFill {
    pub id: String,
    pub cloid: String,
    pub role: FillRole,
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub fee_usdc: Decimal,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BracketEvent {
    Initialized {
        spec: BracketSpec,
    },
    PlanApproved {
        spec_digest: String,
        expires_at_ms: i64,
    },
    ConfigurationVerified {
        isolated: bool,
        leverage: u32,
    },
    EntryReserved {
        nonce: u64,
    },
    SubmissionUncertain,
    Observed {
        observation: BracketObservation,
    },
    Fill {
        fill: BracketFill,
    },
    Funding {
        id: String,
        #[serde(with = "rust_decimal::serde::str")]
        usdc: Decimal,
    },
    ExitsChanging,
    ProtectionRepairReserved {
        take_profit_cloid: Option<String>,
        stop_loss_cloid: Option<String>,
    },
    ExitRequested,
    AgentRevoked,
}
#[derive(Debug, Clone, Serialize)]
pub struct BracketManager {
    pub spec: BracketSpec,
    pub state: BracketState,
    pub reason: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub remaining: Decimal,
    pub last_observation_ms: Option<i64>,
    approval_expires: Option<i64>,
    reserved_nonce: Option<u64>,
    agent_revoked: bool,
    last_event_ms: i64,
    next_sequence: u64,
    fills: BTreeMap<String, BracketFill>,
    funding: BTreeMap<String, Decimal>,
    replacement_tp_cloid: Option<String>,
    replacement_sl_cloid: Option<String>,
    repair_allowed: bool,
}
impl BracketManager {
    /// Current journal-owned TP/SL identities, preserving the original specification.
    pub fn exit_cloids(&self) -> (&str, &str) {
        (
            self.replacement_tp_cloid
                .as_deref()
                .unwrap_or(&self.spec.tp_cloid),
            self.replacement_sl_cloid
                .as_deref()
                .unwrap_or(&self.spec.sl_cloid),
        )
    }
    pub(crate) fn entry_nonce(&self) -> Option<u64> {
        self.reserved_nonce
    }
    pub(crate) fn entry_was_reserved(&self) -> bool {
        self.reserved_nonce.is_some()
    }
    /// Start a lifecycle in a new, dedicated journal.
    ///
    /// # Errors
    /// Rejects invalid specifications/timestamps and failed journal writes.
    pub fn create<J: Journal>(spec: BracketSpec, journal: &mut J, now: i64) -> Result<Self> {
        spec.validate()?;
        require(now >= 0, "invalid timestamp")?;
        require(
            journal.next_sequence() == Some(0),
            "bracket requires a new dedicated journal",
        )?;
        journal.append(
            now,
            &spec.trade_id,
            JournalEventKind::Bracket {
                event: BracketEvent::Initialized { spec: spec.clone() },
                notification: None,
            },
        )?;
        let mut state = Self::new(spec);
        state.last_event_ms = now;
        Ok(state)
    }
    fn new(spec: BracketSpec) -> Self {
        Self {
            spec,
            state: BracketState::Draft,
            reason: String::new(),
            remaining: Decimal::ZERO,
            last_observation_ms: None,
            approval_expires: None,
            reserved_nonce: None,
            agent_revoked: false,
            last_event_ms: 0,
            next_sequence: 1,
            fills: BTreeMap::new(),
            funding: BTreeMap::new(),
            replacement_tp_cloid: None,
            replacement_sl_cloid: None,
            repair_allowed: false,
        }
    }
    /// Validate and durably record an event before updating in-memory state.
    ///
    /// # Errors
    /// Rejects invalid transitions, stale/mismatched input and failed journal writes.
    pub fn record<J: Journal>(
        &mut self,
        journal: &mut J,
        now: i64,
        event: BracketEvent,
    ) -> Result<()> {
        require(
            journal.next_sequence() == Some(self.next_sequence),
            "journal does not match lifecycle sequence",
        )?;
        let mut next = self.clone();
        next.apply(now, &event)?;
        next.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| ExecutionError::Validation("sequence overflow".into()))?;
        journal.append(
            now,
            &self.spec.trade_id,
            JournalEventKind::Bracket {
                notification: self.notification(&next, &event, now)?.map(Box::new),
                event,
            },
        )?;
        *self = next;
        Ok(())
    }
    fn notification(
        &self,
        next: &Self,
        event: &BracketEvent,
        now: i64,
    ) -> Result<Option<hypercarry_notifications::NotificationEvent>> {
        use hypercarry_notifications::{Kind, NotificationEvent};
        let kind = match event {
            BracketEvent::Fill { fill }
                if fill.role == FillRole::Entry && !self.fills.contains_key(&fill.id) =>
            {
                Some(Kind::EntryFilled)
            }
            BracketEvent::Funding { id, .. } if !self.funding.contains_key(id) => {
                Some(Kind::FundingSummary)
            }
            BracketEvent::AgentRevoked if !self.agent_revoked => Some(Kind::AgentRevoked),
            _ if next.state != self.state => match next.state {
                BracketState::Open => Some(Kind::ProtectionVerified),
                BracketState::Closed => Some(Kind::PositionClosed),
                BracketState::NeedsProtection => Some(Kind::ProtectionMissing),
                BracketState::ReconciliationRequired | BracketState::Degraded => {
                    Some(Kind::StateUnknown)
                }
                _ => None,
            },
            _ => None,
        };
        kind.map(|kind| {
            Ok(NotificationEvent {
                schema_version: 1,
                id: hypercarry_notifications::digest(&(
                    "bracket-alert-v1",
                    &self.spec,
                    self.next_sequence,
                    kind,
                ))
                .map_err(|_| ExecutionError::Validation("alert identity encoding failed".into()))?,
                network: self.spec.network.to_string(),
                trade_id: self.spec.trade_id.clone(),
                coin: self.spec.coin.clone(),
                kind,
                observed_at_ms: u64::try_from(now)
                    .map_err(|_| ExecutionError::Validation("alert time invalid".into()))?,
                remaining: next.last_observation_ms.map(|_| next.remaining),
            })
        })
        .transpose()
    }
    /// Replay requires a journal starting with the exact immutable trade identity.
    /// # Errors
    /// Rejects mixed, malformed or inconsistent journals and invalid transitions.
    pub fn replay(records: &[JournalEvent]) -> Result<Self> {
        let Some(JournalEvent {
            event:
                JournalEventKind::Bracket {
                    event: BracketEvent::Initialized { spec },
                    ..
                },
            ..
        }) = records.first()
        else {
            return Err(ExecutionError::Validation(
                "missing bracket initialization".into(),
            ));
        };
        spec.validate()?;
        let mut state = Self::new(spec.clone());
        let mut last_time = -1;
        for (i, r) in records.iter().enumerate() {
            require(
                r.schema_version == 1
                    && r.sequence == i as u64
                    && r.correlation_id == spec.trade_id
                    && r.recorded_at_ms >= 0
                    && r.recorded_at_ms >= last_time,
                "journal identity, sequence or time mismatch",
            )?;
            last_time = r.recorded_at_ms;
            let JournalEventKind::Bracket { event, .. } = &r.event else {
                return Err(ExecutionError::Validation("mixed journal types".into()));
            };
            if i > 0 {
                state.apply(r.recorded_at_ms, event)?;
            } else {
                state.last_event_ms = r.recorded_at_ms;
            }
        }
        state.next_sequence = u64::try_from(records.len())
            .map_err(|_| ExecutionError::Validation("sequence overflow".into()))?;
        // Never report stale protection as current following a process restart.
        if !matches!(state.state, BracketState::Draft | BracketState::Closed) {
            state.state = BracketState::ReconciliationRequired;
            state.reason = "restart requires fresh venue reconciliation".into();
        }
        Ok(state)
    }
    // Keep the event transition table together for review.
    #[allow(clippy::too_many_lines)]
    fn apply(&mut self, now: i64, event: &BracketEvent) -> Result<()> {
        require(now >= self.last_event_ms, "non-monotonic event time")?;
        self.last_event_ms = now;
        match event {
            BracketEvent::Initialized { .. } => {
                return Err(ExecutionError::Validation(
                    "duplicate initialization".into(),
                ));
            }
            BracketEvent::PlanApproved {
                spec_digest,
                expires_at_ms,
            } => {
                require(
                    self.state == BracketState::Draft
                        && *spec_digest == self.spec.digest()?
                        && *expires_at_ms > now
                        && expires_at_ms.checked_sub(now).is_some_and(|v| v <= 60_000),
                    "approval invalid or not draft",
                )?;
                self.approval_expires = Some(*expires_at_ms);
                self.state = BracketState::Approved;
            }
            BracketEvent::ConfigurationVerified { isolated, leverage } => {
                require(
                    self.state == BracketState::Approved && *isolated && *leverage == 1,
                    "initial local workflow requires verified isolated 1x",
                )?;
                self.state = BracketState::Configured;
            }
            BracketEvent::EntryReserved { nonce } => {
                require(
                    !self.agent_revoked
                        && self.state == BracketState::Configured
                        && self.reserved_nonce.is_none()
                        && self.approval_expires.is_some_and(|t| t > now),
                    "entry is unapproved, expired, revoked or already reserved",
                )?;
                require(
                    i64::try_from(*nonce).ok() == Some(now),
                    "entry nonce must match reservation time",
                )?;
                self.reserved_nonce = Some(*nonce);
                self.state = BracketState::EntryPending;
            }
            BracketEvent::SubmissionUncertain => {
                require(self.reserved_nonce.is_some(), "no reserved entry")?;
                self.state = BracketState::ReconciliationRequired;
                self.reason = "submission uncertain; never blindly retry entry".into();
            }
            BracketEvent::Observed { observation } => self.observe(now, observation)?,
            BracketEvent::Fill { fill } => {
                require(
                    self.reserved_nonce.is_some(),
                    "cannot adopt a fill without owned entry",
                )?;
                require(
                    !fill.id.is_empty()
                        && fill.quantity > Decimal::ZERO
                        && fill.price > Decimal::ZERO,
                    "invalid fill",
                )?;
                require(
                    match fill.role {
                        FillRole::Entry => fill.cloid == self.spec.entry_cloid,
                        FillRole::Exit => {
                            fill.cloid == self.spec.tp_cloid
                                || fill.cloid == self.spec.sl_cloid
                                || self.replacement_tp_cloid.as_ref() == Some(&fill.cloid)
                                || self.replacement_sl_cloid.as_ref() == Some(&fill.cloid)
                        }
                    },
                    "unowned fill",
                )?;
                if let Some(old) = self.fills.get(&fill.id) {
                    require(old == fill, "conflicting duplicate fill")?;
                    return Ok(());
                }
                self.fills.insert(fill.id.clone(), fill.clone());
                let q = self
                    .fills
                    .values()
                    .filter(|f| f.role == FillRole::Entry)
                    .try_fold(Decimal::ZERO, |a, f| decimal(a.checked_add(f.quantity)))?;
                require(q <= self.spec.quantity, "entry overfilled planned maximum")?;
                let closed = self
                    .fills
                    .values()
                    .filter(|f| f.role == FillRole::Exit)
                    .try_fold(Decimal::ZERO, |a, f| decimal(a.checked_add(f.quantity)))?;
                require(
                    closed <= q,
                    "exit fills exceed recorded entries; reconcile history first",
                )?;
            }
            BracketEvent::Funding { id, usdc } => {
                require(
                    self.reserved_nonce.is_some() && !id.is_empty(),
                    "invalid funding observation",
                )?;
                if let Some(old) = self.funding.get(id) {
                    require(old == usdc, "conflicting funding duplicate")?;
                } else {
                    self.funding.insert(id.clone(), *usdc);
                }
            }
            BracketEvent::ExitsChanging => {
                require(self.remaining > Decimal::ZERO, "no exposure to protect")?;
                self.state = BracketState::NeedsProtection;
                self.reason = "exit amendment needs fresh confirmation".into();
            }
            BracketEvent::ProtectionRepairReserved {
                take_profit_cloid,
                stop_loss_cloid,
            } => {
                require(
                    (take_profit_cloid.is_none() || self.replacement_tp_cloid.is_none())
                        && (stop_loss_cloid.is_none() || self.replacement_sl_cloid.is_none()),
                    "one repair reservation per exit; further changes require separate review",
                )?;
                require(
                    self.repair_allowed
                        && !self.agent_revoked
                        && self.last_observation_ms.is_some_and(|t| now - t <= 10_000)
                        && (take_profit_cloid.is_some() || stop_loss_cloid.is_some()),
                    "repair requires fresh terminal, bounded exposure",
                )?;
                let mut ids = std::collections::BTreeSet::from([
                    self.spec.entry_cloid.clone(),
                    self.spec.tp_cloid.clone(),
                    self.spec.sl_cloid.clone(),
                ]);
                ids.extend(self.replacement_tp_cloid.clone());
                ids.extend(self.replacement_sl_cloid.clone());
                for id in [take_profit_cloid, stop_loss_cloid].into_iter().flatten() {
                    require(
                        id.len() == 34
                            && id.starts_with("0x")
                            && id[2..].bytes().all(|b| b.is_ascii_hexdigit())
                            && ids.insert(id.to_lowercase()),
                        "invalid or reused repair id",
                    )?;
                }
                if let Some(id) = take_profit_cloid {
                    self.replacement_tp_cloid = Some(id.clone());
                }
                if let Some(id) = stop_loss_cloid {
                    self.replacement_sl_cloid = Some(id.clone());
                }
                self.repair_allowed = false;
                self.state = BracketState::NeedsProtection;
            }
            BracketEvent::ExitRequested => {
                require(self.remaining > Decimal::ZERO, "no exposure to close")?;
                self.state = BracketState::ExitPending;
            }
            BracketEvent::AgentRevoked => {
                self.agent_revoked = true;
                self.state = BracketState::Degraded;
                self.reason = "agent revoked; observe and alert, no signing authority".into();
            }
        }
        Ok(())
    }
    fn observe(&mut self, now: i64, o: &BracketObservation) -> Result<()> {
        require(
            self.reserved_nonce.is_some(),
            "manual positions cannot be silently adopted",
        )?;
        require(
            o.account == self.spec.account
                && o.network == self.spec.network
                && o.coin == self.spec.coin,
            "observation identity mismatch",
        )?;
        require(
            o.complete
                && o.started_at_ms >= 0
                && o.completed_at_ms >= o.started_at_ms
                && o.completed_at_ms <= now
                && now - o.started_at_ms <= 10_000,
            "incomplete or stale observation",
        )?;
        require(
            self.last_observation_ms
                .is_none_or(|t| o.started_at_ms >= t),
            "out-of-order observation",
        )?;
        self.last_observation_ms = Some(o.completed_at_ms);
        self.remaining = o.signed_position.abs();
        let direction_ok = if self.spec.entry_side == Side::Sell {
            o.signed_position <= Decimal::ZERO
        } else {
            o.signed_position >= Decimal::ZERO
        };
        self.reason.clear();
        self.repair_allowed = false;
        if self.agent_revoked {
            self.state = BracketState::Degraded;
            self.reason =
                "agent revoked; observed protection does not restore signing authority".into();
        } else if !direction_ok || self.remaining > self.spec.quantity || o.unmanaged_orders > 0 {
            self.state = BracketState::Degraded;
            self.reason = "unexpected direction, size or unmanaged orders".into();
        } else if o.entry_outcome == EntryOutcome::Unknown {
            self.state = BracketState::ReconciliationRequired;
            self.reason = "entry outcome is not authoritative".into();
        } else if self.remaining.is_zero() {
            self.state =
                if o.entry_outcome == EntryOutcome::Terminal && o.tp.is_none() && o.sl.is_none() {
                    BracketState::Closed
                } else {
                    BracketState::ReconciliationRequired
                };
            if self.state != BracketState::Closed {
                self.reason = "entry still pending or orphan exits remain".into();
            }
        } else if o.entry_outcome == EntryOutcome::Pending {
            self.state = BracketState::EntryPending;
            self.reason =
                "partial fill with live entry; reconcile before claiming final protection".into();
        } else if self.exit_valid(o.tp.as_ref(), true) && self.exit_valid(o.sl.as_ref(), false) {
            self.state = BracketState::Open;
        } else {
            self.state = BracketState::NeedsProtection;
            self.repair_allowed = true;
            self.reason = "missing or mismatched protective exits".into();
        }
        Ok(())
    }
    fn exit_valid(&self, o: Option<&ExitObservation>, tp: bool) -> bool {
        o.is_some_and(|o| {
            &o.cloid
                == if tp {
                    self.replacement_tp_cloid
                        .as_ref()
                        .unwrap_or(&self.spec.tp_cloid)
                } else {
                    self.replacement_sl_cloid
                        .as_ref()
                        .unwrap_or(&self.spec.sl_cloid)
                }
                && o.coin == self.spec.coin
                && o.side != self.spec.entry_side
                && o.quantity == self.remaining
                && o.reduce_only
                && o.is_trigger
                && o.is_take_profit == tp
                && o.trigger == if tp { self.spec.tp } else { self.spec.sl }
        })
    }
    /// Signed cash flows less fees plus funding; meaningful as final P&L only
    /// after flatness and complete fill/funding history have been established.
    /// # Errors
    /// Fails on decimal overflow.
    pub fn recorded_cash_flow(&self) -> Result<Decimal> {
        let mut total = Decimal::ZERO;
        for f in self.fills.values() {
            let notional = decimal(f.quantity.checked_mul(f.price))?;
            let is_sell = if f.role == FillRole::Entry {
                self.spec.entry_side == Side::Sell
            } else {
                self.spec.entry_side == Side::Buy
            };
            total = if is_sell {
                decimal(total.checked_add(notional))?
            } else {
                decimal(total.checked_sub(notional))?
            };
            total = decimal(total.checked_sub(f.fee_usdc))?;
        }
        for flow in self.funding.values() {
            total = decimal(total.checked_add(*flow))?;
        }
        Ok(total)
    }
    /// Operator guidance only: never account-wide cancel protective exits.
    pub fn recovery_guidance(&self) -> &'static str {
        match self.state {
            BracketState::Open => "retain verified native exits; freeze new entry on host failure",
            BracketState::NeedsProtection => {
                "reconcile immediately; repair exits or explicitly approve a bounded close"
            }
            BracketState::Closed => "verify complete fills/funding and finalize journal",
            _ => {
                "freeze new risk; reconcile by original identifiers; do not blindly retry or cancel all exits"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileJournal, InMemoryJournal, read_journal};
    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }
    fn spec() -> BracketSpec {
        BracketSpec {
            trade_id: "fixture-001".into(),
            plan_digest: "ab".repeat(32),
            network: ExecutionNetwork::Testnet,
            account: format!("0x{}", "12".repeat(20)),
            coin: "EXAMPLE".into(),
            entry_side: Side::Sell,
            quantity: d("2"),
            tp: d("40"),
            sl: d("60"),
            entry_cloid: format!("0x{}", "11".repeat(16)),
            tp_cloid: format!("0x{}", "22".repeat(16)),
            sl_cloid: format!("0x{}", "33".repeat(16)),
        }
    }
    fn reserve<J: Journal>(j: &mut J) -> BracketManager {
        let mut m = BracketManager::create(spec(), j, 1000).unwrap();
        m.record(
            j,
            1001,
            BracketEvent::PlanApproved {
                spec_digest: m.spec.digest().unwrap(),
                expires_at_ms: 2000,
            },
        )
        .unwrap();
        m.record(
            j,
            1002,
            BracketEvent::ConfigurationVerified {
                isolated: true,
                leverage: 1,
            },
        )
        .unwrap();
        m.record(j, 1003, BracketEvent::EntryReserved { nonce: 1003 })
            .unwrap();
        m
    }
    fn observation(m: &BracketManager, q: &str) -> BracketObservation {
        let exit = |tp| ExitObservation {
            cloid: if tp {
                m.spec.tp_cloid.clone()
            } else {
                m.spec.sl_cloid.clone()
            },
            coin: m.spec.coin.clone(),
            side: Side::Buy,
            quantity: d(q).abs(),
            trigger: if tp { d("40") } else { d("60") },
            reduce_only: true,
            is_trigger: true,
            is_take_profit: tp,
        };
        BracketObservation {
            account: m.spec.account.clone(),
            network: m.spec.network,
            coin: m.spec.coin.clone(),
            entry_outcome: EntryOutcome::Terminal,
            signed_position: d(q),
            tp: Some(exit(true)),
            sl: Some(exit(false)),
            unmanaged_orders: 0,
            started_at_ms: 1004,
            completed_at_ms: 1005,
            complete: true,
        }
    }
    #[test]
    fn repair_reservation_preserves_real_client_identity() {
        let mut j = InMemoryJournal::default();
        let mut m = reserve(&mut j);
        let mut o = observation(&m, "-1");
        o.sl = None;
        m.record(&mut j, 1005, BracketEvent::Observed { observation: o })
            .unwrap();
        let replacement = format!("0x{}", "44".repeat(16));
        m.record(
            &mut j,
            1006,
            BracketEvent::ProtectionRepairReserved {
                take_profit_cloid: None,
                stop_loss_cloid: Some(replacement.clone()),
            },
        )
        .unwrap();
        let mut o = observation(&m, "-1");
        o.started_at_ms = 1006;
        o.completed_at_ms = 1007;
        o.sl.as_mut().unwrap().cloid = replacement;
        m.record(&mut j, 1007, BracketEvent::Observed { observation: o })
            .unwrap();
        assert_eq!(m.state, BracketState::Open);
        let replayed = BracketManager::replay(&j.events).unwrap();
        assert_eq!(replayed.exit_cloids(), m.exit_cloids());
        assert_eq!(replayed.spec.sl_cloid, format!("0x{}", "33".repeat(16)));
        assert!(
            m.record(
                &mut j,
                1008,
                BracketEvent::ProtectionRepairReserved {
                    take_profit_cloid: None,
                    stop_loss_cloid: Some(format!("0x{}", "55".repeat(16))),
                }
            )
            .is_err()
        );
    }
    #[test]
    fn partial_fill_and_missing_exit_cannot_claim_protection() {
        let mut j = InMemoryJournal::default();
        let mut m = reserve(&mut j);
        let mut o = observation(&m, "-1");
        o.sl = None;
        m.record(&mut j, 1005, BracketEvent::Observed { observation: o })
            .unwrap();
        assert_eq!(m.state, BracketState::NeedsProtection);
        let mut o = observation(&m, "-1");
        o.started_at_ms = 1005;
        m.record(&mut j, 1005, BracketEvent::Observed { observation: o })
            .unwrap();
        assert_eq!(m.state, BracketState::Open);
        assert!(m.recovery_guidance().contains("retain"));
    }
    #[test]
    fn lost_ack_and_restart_never_allow_duplicate_entry() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("journal.jsonl");
        {
            let mut j = FileJournal::open(&p).unwrap();
            let mut m = reserve(&mut j);
            m.record(&mut j, 1004, BracketEvent::SubmissionUncertain)
                .unwrap();
        }
        let mut m = BracketManager::replay(&read_journal(&p).unwrap()).unwrap();
        assert_eq!(m.state, BracketState::ReconciliationRequired);
        let mut j = FileJournal::open(&p).unwrap();
        assert!(
            m.record(&mut j, 1005, BracketEvent::EntryReserved { nonce: 1005 })
                .is_err()
        );
    }
    #[test]
    fn orphan_exit_blocks_closed_state() {
        let mut j = InMemoryJournal::default();
        let mut m = reserve(&mut j);
        let mut o = observation(&m, "0");
        m.record(
            &mut j,
            1005,
            BracketEvent::Observed {
                observation: o.clone(),
            },
        )
        .unwrap();
        assert_ne!(m.state, BracketState::Closed);
        o.tp = None;
        o.sl = None;
        o.started_at_ms = 1005;
        m.record(&mut j, 1005, BracketEvent::Observed { observation: o })
            .unwrap();
        assert_eq!(m.state, BracketState::Closed);
    }
    #[test]
    fn duplicate_fills_and_funding_do_not_double_count_cash() {
        let mut j = InMemoryJournal::default();
        let mut m = reserve(&mut j);
        let entry = BracketFill {
            id: "entry".into(),
            cloid: m.spec.entry_cloid.clone(),
            role: FillRole::Entry,
            quantity: d("2"),
            price: d("50"),
            fee_usdc: d("0.05"),
        };
        for _ in 0..2 {
            m.record(
                &mut j,
                1004,
                BracketEvent::Fill {
                    fill: entry.clone(),
                },
            )
            .unwrap();
        }
        let exit = BracketFill {
            id: "exit".into(),
            cloid: m.spec.tp_cloid.clone(),
            role: FillRole::Exit,
            quantity: d("2"),
            price: d("40"),
            fee_usdc: d("0.04"),
        };
        m.record(&mut j, 1005, BracketEvent::Fill { fill: exit })
            .unwrap();
        for _ in 0..2 {
            m.record(
                &mut j,
                1006,
                BracketEvent::Funding {
                    id: "funding".into(),
                    usdc: d("-0.01"),
                },
            )
            .unwrap();
        }
        assert_eq!(m.recorded_cash_flow().unwrap(), d("19.90"));
    }
    #[test]
    fn stale_wrong_identity_and_manual_changes_fail_closed() {
        let mut j = InMemoryJournal::default();
        let mut m = reserve(&mut j);
        let o = observation(&m, "-1");
        assert!(
            m.record(
                &mut j,
                20000,
                BracketEvent::Observed {
                    observation: o.clone()
                }
            )
            .is_err()
        );
        let mut bad = o.clone();
        bad.account = format!("0x{}", "34".repeat(20));
        assert!(
            m.record(&mut j, 1005, BracketEvent::Observed { observation: bad })
                .is_err()
        );
        let mut manual = o;
        manual.signed_position = d("3");
        m.record(
            &mut j,
            1005,
            BracketEvent::Observed {
                observation: manual,
            },
        )
        .unwrap();
        assert_eq!(m.state, BracketState::Degraded);
    }

    #[test]
    fn revoked_agent_cannot_be_restored_by_valid_observation() {
        let mut j = InMemoryJournal::default();
        let mut m = reserve(&mut j);
        m.record(&mut j, 1004, BracketEvent::AgentRevoked).unwrap();
        let o = observation(&m, "-1");
        m.record(&mut j, 1005, BracketEvent::Observed { observation: o })
            .unwrap();
        assert_eq!(m.state, BracketState::Degraded);
        assert!(m.reason.contains("revoked"));
        assert!(
            m.record(&mut j, 1006, BracketEvent::EntryReserved { nonce: 1006 })
                .is_err()
        );
    }

    #[test]
    fn journal_failure_does_not_advance_memory() {
        struct Broken;
        impl Journal for Broken {
            fn next_sequence(&self) -> Option<u64> {
                Some(4)
            }
            fn append(&mut self, _: i64, _: &str, _: JournalEventKind) -> Result<JournalEvent> {
                Err(ExecutionError::Validation("disk unavailable".into()))
            }
        }
        let mut j = InMemoryJournal::default();
        let mut m = reserve(&mut j);
        let before = serde_json::to_value(&m).unwrap();
        assert!(
            m.record(&mut Broken, 1004, BracketEvent::SubmissionUncertain)
                .is_err()
        );
        assert_eq!(before, serde_json::to_value(&m).unwrap());
        assert!(
            m.record(&mut j, 999, BracketEvent::SubmissionUncertain)
                .is_err()
        );
        assert_eq!(before, serde_json::to_value(&m).unwrap());
    }

    #[test]
    fn reused_or_wrong_journal_is_rejected_before_mutation() {
        let mut j = InMemoryJournal::default();
        let mut m = reserve(&mut j);
        let before = j.events.clone();
        assert!(BracketManager::create(spec(), &mut j, 1005).is_err());
        assert_eq!(before, j.events);
        let mut wrong = InMemoryJournal::default();
        assert!(
            m.record(&mut wrong, 1005, BracketEvent::SubmissionUncertain)
                .is_err()
        );
        assert_eq!(wrong.events, Vec::<JournalEvent>::new());
    }
}
