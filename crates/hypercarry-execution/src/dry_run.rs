use crate::{
    ExecutionAdapter, ExecutionError, ExecutionMode, ExecutionReport, Journal, JournalEventKind,
    MarketMetadataResolver, OrderIntent, OrderState, Rejection, RiskDecision, RiskPolicy,
    ValidatedOrder,
};

/// Non-signing adapter. It records success without contacting a venue.
#[derive(Debug, Default)]
pub struct DryRunAdapter;

impl ExecutionAdapter for DryRunAdapter {
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::DryRun
    }

    fn execute(&mut self, order: &ValidatedOrder) -> Result<ExecutionReport, ExecutionError> {
        order.validate()?;
        Ok(ExecutionReport::empty(
            order,
            ExecutionMode::DryRun,
            OrderState::DryRunRecorded,
        ))
    }
}

/// Metadata-resolution, quantization, risk, journal, and adapter pipeline.
///
/// This type has no signer or transport field. Its exact validated action is
/// durably appended before the non-signing adapter runs.
pub struct DryRun<R, P, J> {
    resolver: R,
    policy: P,
    journal: J,
    adapter: DryRunAdapter,
}

impl<R, P, J> DryRun<R, P, J>
where
    R: MarketMetadataResolver,
    P: RiskPolicy,
    J: Journal,
{
    /// Assemble a dry-run pipeline from a resolver, policy, and journal.
    pub fn new(resolver: R, policy: P, journal: J) -> Self {
        Self {
            resolver,
            policy,
            journal,
            adapter: DryRunAdapter,
        }
    }

    /// Resolves and journals the exact action without signing or submission.
    ///
    /// # Errors
    ///
    /// Returns an error when metadata, quantization, policy evaluation, or a
    /// durable journal append fails.
    pub fn evaluate(&mut self, intent: &OrderIntent) -> Result<ExecutionReport, ExecutionError> {
        let metadata = self.resolver.resolve(&intent.venue, &intent.market)?;
        let order = ValidatedOrder::resolve(intent, &metadata)?;
        let decision = self.policy.evaluate(&order)?;
        self.journal.append(
            intent.created_at_ms,
            &intent.correlation_id,
            decision.clone().into(),
        )?;
        if let RiskDecision::Reject { code, reason } = decision {
            let mut report =
                ExecutionReport::empty(&order, ExecutionMode::DryRun, OrderState::Rejected);
            report.rejection = Some(Rejection { code, reason });
            self.journal.append(
                intent.created_at_ms,
                &intent.correlation_id,
                JournalEventKind::StateObserved {
                    state: OrderState::Rejected,
                },
            )?;
            return Ok(report);
        }
        self.journal.append(
            intent.created_at_ms,
            &intent.correlation_id,
            JournalEventKind::ExactAction {
                mode: self.adapter.mode(),
                order: order.clone(),
            },
        )?;
        let report = self.adapter.execute(&order)?;
        self.journal.append(
            intent.created_at_ms,
            &intent.correlation_id,
            JournalEventKind::StateObserved {
                state: report.state,
            },
        )?;
        Ok(report)
    }

    /// Consume the pipeline and return its journal.
    pub fn into_journal(self) -> J {
        self.journal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryJournal, MarketMetadata};
    use rust_decimal::Decimal;

    struct Resolver;

    impl MarketMetadataResolver for Resolver {
        fn resolve(&self, venue: &str, market: &str) -> Result<MarketMetadata, ExecutionError> {
            MarketMetadata::new(venue, market, dec("0.5"), dec("0.1"), dec("0.1"))
        }
    }

    struct Allow;

    impl RiskPolicy for Allow {
        fn evaluate(&self, _order: &ValidatedOrder) -> Result<RiskDecision, ExecutionError> {
            Ok(RiskDecision::Allow {
                code: "fixture_allow".to_owned(),
                reason: "fixture limits satisfied".to_owned(),
            })
        }
    }

    struct Reject;

    impl RiskPolicy for Reject {
        fn evaluate(&self, _order: &ValidatedOrder) -> Result<RiskDecision, ExecutionError> {
            Ok(RiskDecision::Reject {
                code: "test_limit".to_owned(),
                reason: "rejected by fixture policy".to_owned(),
            })
        }
    }

    struct FailClosed;

    impl RiskPolicy for FailClosed {
        fn evaluate(&self, _order: &ValidatedOrder) -> Result<RiskDecision, ExecutionError> {
            Err(ExecutionError::Policy(
                "risk snapshot unavailable".to_owned(),
            ))
        }
    }

    #[test]
    fn dry_run_records_exact_quantized_action_without_submission() {
        let intent = intent();
        let mut dry_run = DryRun::new(Resolver, Allow, InMemoryJournal::default());
        let report = dry_run.evaluate(&intent).unwrap();
        assert_eq!(report.state, OrderState::DryRunRecorded);
        let journal = dry_run.into_journal();
        assert_eq!(journal.events.len(), 3);
        let JournalEventKind::ExactAction { mode, order } = &journal.events[1].event else {
            panic!("second event must be exact action");
        };
        assert_eq!(*mode, ExecutionMode::DryRun);
        assert_eq!(order.quantity, dec("1.2"));
        assert_eq!(order.limit_price, dec("100"));
    }

    #[test]
    fn rejected_dry_run_never_records_an_action() {
        let mut dry_run = DryRun::new(Resolver, Reject, InMemoryJournal::default());
        let report = dry_run.evaluate(&intent()).unwrap();
        assert_eq!(report.state, OrderState::Rejected);
        assert!(
            dry_run
                .into_journal()
                .events
                .iter()
                .all(|event| !matches!(event.event, JournalEventKind::ExactAction { .. }))
        );
    }

    #[test]
    fn policy_error_fails_closed_before_any_action_is_journaled() {
        let mut dry_run = DryRun::new(Resolver, FailClosed, InMemoryJournal::default());
        let error = dry_run.evaluate(&intent()).unwrap_err();
        assert!(error.to_string().contains("risk snapshot unavailable"));
        assert!(dry_run.into_journal().events.is_empty());
    }

    fn intent() -> OrderIntent {
        OrderIntent::limit(
            "intent-1",
            "venue",
            "BTC-PERP",
            crate::Side::Buy,
            dec("1.29"),
            dec("100.49"),
            1_000,
        )
        .unwrap()
    }

    fn dec(value: &str) -> Decimal {
        value.parse().unwrap()
    }
}
