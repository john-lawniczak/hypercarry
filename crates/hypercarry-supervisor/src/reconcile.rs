//! Conservative single-canary reconciliation. Only an unsubmitted exact action
//! can become placement-ready; a previously submitted action requires recovery.
use anyhow::{Result, ensure};
use hypercarry_execution::{
    ClientOrderId, ExecutionMode, ExecutionNetwork, JournalEvent, JournalEventKind, OrderState,
    OrderStateMachine,
};
use hypercarry_mainnet_config::RuntimeConfig;
use serde_json::Value;

pub fn before_submission(
    events: &[JournalEvent],
    runtime: &RuntimeConfig,
    status: &Value,
) -> Result<bool> {
    // Query the deterministic client ID even for an empty journal. A lost local
    // journal must not turn an existing venue order into a fresh canary.
    if status["status"] != "unknownOid" {
        return Ok(false);
    }
    let mut actions = 0;
    for (index, record) in events.iter().enumerate() {
        ensure!(
            record.correlation_id == runtime.order.correlation_id,
            "journal contains another order"
        );
        if index == 0 {
            ensure!(
                matches!(&record.event, JournalEventKind::ExecutionContextBound {
                network: ExecutionNetwork::Mainnet, account_id, config_digest, signer_key_id, ..
            } if account_id == &runtime.account_address && config_digest == &runtime.digest()? && signer_key_id == &runtime.signer_alias),
                "journal identity mismatch"
            );
        }
        match &record.event {
            JournalEventKind::ExecutionContextBound { .. } => {
                ensure!(index == 0, "duplicate journal binding");
            }
            JournalEventKind::ExactAction { mode, order } => {
                ensure!(
                    *mode == ExecutionMode::Mainnet && order == &runtime.order,
                    "unreviewed journal action"
                );
                actions += 1;
                ensure!(actions == 1, "repeated journal action");
            }
            JournalEventKind::LifecycleTransition { .. } => {
                ensure!(actions == 1, "transition without action");
            }
            JournalEventKind::RiskDecisionRecorded { allowed: true, .. }
            | JournalEventKind::RiskAllowed => {}
            // Rejected/uncertain/legacy state requires operator reconciliation.
            _ => return Ok(false),
        }
    }
    let id = ClientOrderId::derive(&runtime.order)?;
    let state = OrderStateMachine::recover_from_journal(runtime.order.clone(), id, events)?;
    Ok(state.state() == OrderState::Validated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercarry_execution::{InMemoryJournal, Journal};
    use serde_json::json;
    #[test]
    fn lost_journal_never_hides_existing_venue_order() {
        let runtime = serde_json::from_str(include_str!(
            "../../../docs/mainnet-runtime-config.example.json"
        ))
        .unwrap();
        assert!(before_submission(&[], &runtime, &json!({"status":"unknownOid"})).unwrap());
        assert!(!before_submission(&[], &runtime, &json!({"status":"order"})).unwrap());
        assert!(!before_submission(&[], &runtime, &json!({})).unwrap());
    }
    #[test]
    fn pre_signing_action_is_ready_but_inflight_or_wrong_binding_is_not() {
        let runtime: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../docs/mainnet-runtime-config.example.json"
        ))
        .unwrap();
        let mut journal = InMemoryJournal::default();
        let correlation = &runtime.order.correlation_id;
        journal
            .append(
                1,
                correlation,
                JournalEventKind::ExecutionContextBound {
                    network: ExecutionNetwork::Mainnet,
                    account_id: runtime.account_address.clone(),
                    config_digest: runtime.digest().unwrap(),
                    evidence_digest: "a".repeat(64),
                    signer_key_id: runtime.signer_alias.clone(),
                },
            )
            .unwrap();
        journal
            .append(
                2,
                correlation,
                JournalEventKind::ExactAction {
                    mode: ExecutionMode::Mainnet,
                    order: runtime.order.clone(),
                },
            )
            .unwrap();
        let unknown = json!({"status":"unknownOid"});
        assert!(before_submission(&journal.events, &runtime, &unknown).unwrap());
        let mut machine = OrderStateMachine::new(
            runtime.order.clone(),
            ClientOrderId::derive(&runtime.order).unwrap(),
        );
        let hypercarry_execution::TransitionOutcome::Applied(transition) = machine
            .transition(
                OrderState::SubmissionPending,
                3,
                None,
                rust_decimal::Decimal::ZERO,
            )
            .unwrap()
        else {
            panic!()
        };
        journal
            .append(
                3,
                correlation,
                JournalEventKind::LifecycleTransition { transition },
            )
            .unwrap();
        assert!(!before_submission(&journal.events, &runtime, &unknown).unwrap());
        let mut other = runtime.clone();
        other.signer_alias = "other".into();
        assert!(before_submission(&journal.events, &other, &unknown).is_err());
    }
}
