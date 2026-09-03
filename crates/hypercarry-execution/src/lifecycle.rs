use crate::{
    ClientOrderId, ExecutionError, JournalEvent, JournalEventKind, OrderState, ValidatedOrder,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Durable lifecycle transition. The cumulative fill makes replay independent
/// of duplicate venue events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleTransition {
    /// Durable client order identifier.
    pub client_order_id: ClientOrderId,
    /// State before the transition.
    pub from: OrderState,
    /// State after the transition.
    pub to: OrderState,
    /// Transition time, unix milliseconds.
    pub occurred_at_ms: i64,
    /// Venue-assigned order identifier, once known.
    pub venue_order_id: Option<u64>,
    /// Cumulative filled quantity after the transition.
    #[serde(with = "rust_decimal::serde::str")]
    pub cumulative_filled: Decimal,
}

/// Result of applying a venue observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionOutcome {
    /// The observation advanced state and produced a transition.
    Applied(LifecycleTransition),
    /// The observation was a duplicate and left state unchanged.
    Duplicate,
}

/// Strict, replayable state machine for one validated order.
#[derive(Debug, Clone)]
pub struct OrderStateMachine {
    order: ValidatedOrder,
    client_order_id: ClientOrderId,
    state: OrderState,
    venue_order_id: Option<u64>,
    cumulative_filled: Decimal,
    last_occurred_at_ms: i64,
}

impl OrderStateMachine {
    /// Start a fresh state machine for a validated order in `Validated` state.
    pub fn new(order: ValidatedOrder, client_order_id: ClientOrderId) -> Self {
        let created_at_ms = order.created_at_ms;
        Self {
            order,
            client_order_id,
            state: OrderState::Validated,
            venue_order_id: None,
            cumulative_filled: Decimal::ZERO,
            last_occurred_at_ms: created_at_ms,
        }
    }

    /// Recovers one machine by replaying durable transitions in order.
    ///
    /// # Errors
    ///
    /// Returns an error for an impossible, regressive, mismatched, or
    /// out-of-order transition.
    pub fn recover(
        order: ValidatedOrder,
        client_order_id: ClientOrderId,
        transitions: &[LifecycleTransition],
    ) -> Result<Self, ExecutionError> {
        let mut machine = Self::new(order, client_order_id);
        for expected in transitions {
            if expected.client_order_id != client_order_id || expected.from != machine.state {
                return Err(lifecycle_error(
                    "replayed transition identity or previous state does not match",
                ));
            }
            let TransitionOutcome::Applied(actual) = machine.transition(
                expected.to,
                expected.occurred_at_ms,
                expected.venue_order_id,
                expected.cumulative_filled,
            )?
            else {
                return Err(lifecycle_error(
                    "durable journal contains duplicate transition",
                ));
            };
            if actual != *expected {
                return Err(lifecycle_error(
                    "durable transition does not replay exactly",
                ));
            }
        }
        Ok(machine)
    }

    /// Recovers this order from matching lifecycle records in a validated
    /// durable journal.
    ///
    /// # Errors
    ///
    /// Returns an error when the order identity or any matching transition is
    /// inconsistent.
    pub fn recover_from_journal(
        order: ValidatedOrder,
        client_order_id: ClientOrderId,
        events: &[JournalEvent],
    ) -> Result<Self, ExecutionError> {
        let transitions = events
            .iter()
            .filter(|event| event.correlation_id == order.correlation_id)
            .filter_map(|event| match &event.event {
                JournalEventKind::LifecycleTransition { transition } => Some(transition.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        Self::recover(order, client_order_id, &transitions)
    }

    /// Applies a validated state observation.
    ///
    /// Exact duplicates are idempotent. Any other repeated, regressive, or
    /// impossible transition fails closed.
    ///
    /// # Errors
    ///
    /// Returns an error when timestamps, venue identity, fill quantity, or the
    /// state edge violates lifecycle invariants.
    pub fn transition(
        &mut self,
        to: OrderState,
        occurred_at_ms: i64,
        venue_order_id: Option<u64>,
        cumulative_filled: Decimal,
    ) -> Result<TransitionOutcome, ExecutionError> {
        if to == self.state
            && venue_order_id == self.venue_order_id
            && cumulative_filled == self.cumulative_filled
        {
            return Ok(TransitionOutcome::Duplicate);
        }
        if occurred_at_ms < self.last_occurred_at_ms {
            return Err(lifecycle_error("transition timestamp regressed"));
        }
        if !allowed_transition(self.state, to) {
            return Err(lifecycle_error(format!(
                "impossible transition from {:?} to {to:?}",
                self.state
            )));
        }
        if self.venue_order_id.is_some()
            && venue_order_id.is_some()
            && self.venue_order_id != venue_order_id
        {
            return Err(lifecycle_error("venue order ID changed"));
        }
        if cumulative_filled < self.cumulative_filled
            || cumulative_filled < Decimal::ZERO
            || cumulative_filled > self.order.quantity
        {
            return Err(lifecycle_error(
                "cumulative fill must be monotonic and within order quantity",
            ));
        }
        match to {
            OrderState::PartiallyFilled if cumulative_filled <= Decimal::ZERO => {
                return Err(lifecycle_error("partial fill must be positive"));
            }
            OrderState::PartiallyFilled if cumulative_filled >= self.order.quantity => {
                return Err(lifecycle_error(
                    "partial fill must remain below order quantity",
                ));
            }
            OrderState::Filled if cumulative_filled != self.order.quantity => {
                return Err(lifecycle_error(
                    "filled state requires the complete quantity",
                ));
            }
            OrderState::Open if !cumulative_filled.is_zero() => {
                return Err(lifecycle_error("open state cannot discard a partial fill"));
            }
            _ => {}
        }
        let transition = LifecycleTransition {
            client_order_id: self.client_order_id,
            from: self.state,
            to,
            occurred_at_ms,
            venue_order_id: venue_order_id.or(self.venue_order_id),
            cumulative_filled,
        };
        self.state = to;
        self.venue_order_id = transition.venue_order_id;
        self.cumulative_filled = cumulative_filled;
        self.last_occurred_at_ms = occurred_at_ms;
        Ok(TransitionOutcome::Applied(transition))
    }

    /// Current lifecycle state.
    pub const fn state(&self) -> OrderState {
        self.state
    }

    /// Durable client order identifier.
    pub const fn client_order_id(&self) -> ClientOrderId {
        self.client_order_id
    }

    /// Venue-assigned order identifier, once known.
    pub const fn venue_order_id(&self) -> Option<u64> {
        self.venue_order_id
    }

    /// Cumulative filled quantity observed so far.
    pub const fn cumulative_filled(&self) -> Decimal {
        self.cumulative_filled
    }

    /// The validated order this machine tracks.
    pub fn order(&self) -> &ValidatedOrder {
        &self.order
    }
}

fn allowed_transition(from: OrderState, to: OrderState) -> bool {
    use OrderState::{
        CancelPending, Cancelled, DryRunRecorded, Filled, NoFill, Open, PartiallyFilled, Rejected,
        SubmissionPending, SubmissionUncertain, Validated,
    };
    matches!(
        (from, to),
        (
            Validated,
            SubmissionPending | DryRunRecorded | Rejected | NoFill
        ) | (
            SubmissionPending,
            SubmissionUncertain | Open | PartiallyFilled | Filled | Rejected
        ) | (
            SubmissionUncertain | CancelPending,
            Open | PartiallyFilled | Filled | Cancelled | Rejected
        ) | (
            Open,
            PartiallyFilled | Filled | CancelPending | Cancelled | Rejected
        ) | (PartiallyFilled, Filled | CancelPending | Cancelled)
    )
}

fn lifecycle_error(message: impl Into<String>) -> ExecutionError {
    ExecutionError::Lifecycle(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MarketMetadata, OrderIntent, Side};

    #[test]
    fn complete_lifecycle_replays_exactly() {
        let (order, client_id) = fixture();
        let mut machine = OrderStateMachine::new(order.clone(), client_id);
        let mut transitions = Vec::new();
        for (state, time, oid, filled) in [
            (OrderState::SubmissionPending, 1_001, None, "0"),
            (OrderState::Open, 1_002, Some(42), "0"),
            (OrderState::PartiallyFilled, 1_003, Some(42), "0.4"),
            (OrderState::CancelPending, 1_004, Some(42), "0.4"),
            (OrderState::Cancelled, 1_005, Some(42), "0.4"),
        ] {
            let TransitionOutcome::Applied(event) = machine
                .transition(state, time, oid, filled.parse().unwrap())
                .unwrap()
            else {
                panic!("new transition must apply");
            };
            transitions.push(event);
        }
        let recovered = OrderStateMachine::recover(order, client_id, &transitions).unwrap();
        assert_eq!(recovered.state(), OrderState::Cancelled);
        assert_eq!(recovered.cumulative_filled(), dec("0.4"));
    }

    #[test]
    fn journal_recovery_ignores_other_orders() {
        let (order, client_id) = fixture();
        let mut machine = OrderStateMachine::new(order.clone(), client_id);
        let TransitionOutcome::Applied(transition) = machine
            .transition(OrderState::SubmissionPending, 1_001, None, Decimal::ZERO)
            .unwrap()
        else {
            panic!("transition must apply");
        };
        let events = vec![JournalEvent {
            schema_version: crate::EXECUTION_SCHEMA_VERSION,
            sequence: 0,
            recorded_at_ms: 1_001,
            correlation_id: order.correlation_id.clone(),
            event: JournalEventKind::LifecycleTransition { transition },
        }];
        let recovered = OrderStateMachine::recover_from_journal(order, client_id, &events).unwrap();
        assert_eq!(recovered.state(), OrderState::SubmissionPending);
    }

    #[test]
    fn duplicate_is_idempotent_but_regression_fails_closed() {
        let (order, client_id) = fixture();
        let mut machine = OrderStateMachine::new(order, client_id);
        machine
            .transition(OrderState::SubmissionPending, 1_001, None, Decimal::ZERO)
            .unwrap();
        machine
            .transition(OrderState::Open, 1_002, Some(42), Decimal::ZERO)
            .unwrap();
        assert_eq!(
            machine
                .transition(OrderState::Open, 1_002, Some(42), Decimal::ZERO)
                .unwrap(),
            TransitionOutcome::Duplicate
        );
        assert!(
            machine
                .transition(
                    OrderState::SubmissionPending,
                    1_003,
                    Some(42),
                    Decimal::ZERO
                )
                .is_err()
        );
    }

    #[test]
    fn overfill_and_changed_venue_id_are_rejected() {
        let (order, client_id) = fixture();
        let mut machine = OrderStateMachine::new(order, client_id);
        machine
            .transition(OrderState::SubmissionPending, 1_001, None, Decimal::ZERO)
            .unwrap();
        machine
            .transition(OrderState::Open, 1_002, Some(42), Decimal::ZERO)
            .unwrap();
        assert!(
            machine
                .transition(OrderState::Filled, 1_003, Some(42), dec("1.1"))
                .is_err()
        );
        assert!(
            machine
                .transition(OrderState::CancelPending, 1_003, Some(43), Decimal::ZERO)
                .is_err()
        );
    }

    fn fixture() -> (ValidatedOrder, ClientOrderId) {
        let intent = OrderIntent::limit(
            "lifecycle-1",
            "hyperliquid",
            "BTC",
            Side::Buy,
            dec("1"),
            dec("100"),
            1_000,
        )
        .unwrap();
        let metadata =
            MarketMetadata::new("hyperliquid", "BTC", dec("0.1"), dec("0.1"), dec("0.1")).unwrap();
        let order = ValidatedOrder::resolve(&intent, &metadata).unwrap();
        let client_order_id = ClientOrderId::derive(&order).unwrap();
        (order, client_order_id)
    }

    fn dec(value: &str) -> Decimal {
        value.parse().unwrap()
    }
}
