use crate::ExecutionError;
use std::collections::VecDeque;

/// Admission result from the proactive rolling-window throttle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleDecision {
    /// The request may proceed immediately.
    Allow,
    /// The request must wait before proceeding.
    Wait {
        /// Suggested delay before retrying, milliseconds.
        retry_after_ms: u64,
    },
}

/// Deterministic rolling-window request throttle.
#[derive(Debug, Clone)]
pub struct RequestThrottle {
    max_requests: usize,
    window_ms: i64,
    admitted_at_ms: VecDeque<i64>,
}

impl RequestThrottle {
    /// Creates an empty throttle.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero request limit or non-positive window.
    pub fn new(max_requests: usize, window_ms: i64) -> Result<Self, ExecutionError> {
        if max_requests == 0 || window_ms <= 0 {
            return Err(reliability_error(
                "request throttle requires a positive limit and window",
            ));
        }
        Ok(Self {
            max_requests,
            window_ms,
            admitted_at_ms: VecDeque::with_capacity(max_requests),
        })
    }

    /// Admits and records a request, or returns the exact earliest retry delay.
    ///
    /// # Errors
    ///
    /// Returns an error when time regresses or arithmetic overflows.
    pub fn admit(&mut self, now_ms: i64) -> Result<ThrottleDecision, ExecutionError> {
        if now_ms < 0
            || self
                .admitted_at_ms
                .back()
                .is_some_and(|last| now_ms < *last)
        {
            return Err(reliability_error(
                "request throttle requires monotonic non-negative time",
            ));
        }
        let cutoff = now_ms
            .checked_sub(self.window_ms)
            .ok_or_else(|| reliability_error("request throttle window overflow"))?;
        while self
            .admitted_at_ms
            .front()
            .is_some_and(|time| *time <= cutoff)
        {
            self.admitted_at_ms.pop_front();
        }
        if self.admitted_at_ms.len() < self.max_requests {
            self.admitted_at_ms.push_back(now_ms);
            return Ok(ThrottleDecision::Allow);
        }
        let earliest = *self
            .admitted_at_ms
            .front()
            .ok_or_else(|| reliability_error("throttle capacity invariant failed"))?;
        let available_at = earliest
            .checked_add(self.window_ms)
            .ok_or_else(|| reliability_error("request retry timestamp overflow"))?;
        let delay = available_at
            .checked_sub(now_ms)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| reliability_error("request retry delay is invalid"))?;
        Ok(ThrottleDecision::Wait {
            retry_after_ms: delay,
        })
    }
}

/// Failure classification used to prevent blind submission retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionFailure {
    /// The request is known not to have crossed the local transport boundary.
    TransientBeforeWrite,
    /// The venue may have accepted the request; status must be reconciled.
    UncertainAfterWrite,
    /// The venue rate-limited the request.
    RateLimited {
        /// Suggested delay before retrying, milliseconds, when supplied.
        retry_after_ms: Option<u64>,
    },
    /// The request failed permanently and must not be retried.
    Permanent,
}

/// Required next action after a failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Retry the request after a bounded delay.
    RetryAfter {
        /// Delay before the next attempt, milliseconds.
        delay_ms: u64,
    },
    /// Reconcile venue state before any retry.
    ReconcileBeforeRetry,
    /// Stop retrying.
    Stop,
}

/// Bounded exponential retry policy with no jitter hidden from tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Maximum submission attempts.
    pub max_attempts: u32,
    /// Base backoff before exponential growth, milliseconds.
    pub base_backoff_ms: u64,
    /// Upper bound on backoff, milliseconds.
    pub max_backoff_ms: u64,
}

impl RetryPolicy {
    /// Creates a bounded policy.
    ///
    /// # Errors
    ///
    /// Returns an error for zero attempts/backoff or an inverted cap.
    pub fn new(
        max_attempts: u32,
        base_backoff_ms: u64,
        max_backoff_ms: u64,
    ) -> Result<Self, ExecutionError> {
        if max_attempts == 0 || base_backoff_ms == 0 || max_backoff_ms < base_backoff_ms {
            return Err(reliability_error("invalid bounded retry policy"));
        }
        Ok(Self {
            max_attempts,
            base_backoff_ms,
            max_backoff_ms,
        })
    }

    /// Determines the safe next action after `attempt` (one-based) failed.
    pub fn action(self, attempt: u32, failure: SubmissionFailure) -> RecoveryAction {
        if attempt == 0 || attempt >= self.max_attempts || failure == SubmissionFailure::Permanent {
            return RecoveryAction::Stop;
        }
        if failure == SubmissionFailure::UncertainAfterWrite {
            return RecoveryAction::ReconcileBeforeRetry;
        }
        let exponent = attempt.saturating_sub(1).min(63);
        let exponential = self
            .base_backoff_ms
            .saturating_mul(1_u64 << exponent)
            .min(self.max_backoff_ms);
        let delay_ms = match failure {
            SubmissionFailure::RateLimited { retry_after_ms } => {
                exponential.max(retry_after_ms.unwrap_or(0))
            }
            SubmissionFailure::TransientBeforeWrite => exponential,
            SubmissionFailure::UncertainAfterWrite | SubmissionFailure::Permanent => {
                unreachable!("handled above")
            }
        };
        RecoveryAction::RetryAfter { delay_ms }
    }
}

fn reliability_error(message: impl Into<String>) -> ExecutionError {
    ExecutionError::Reliability(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_enforces_boundary_without_consuming_rejected_slots() {
        let mut throttle = RequestThrottle::new(2, 1_000).unwrap();
        assert_eq!(throttle.admit(1_000).unwrap(), ThrottleDecision::Allow);
        assert_eq!(throttle.admit(1_100).unwrap(), ThrottleDecision::Allow);
        assert_eq!(
            throttle.admit(1_999).unwrap(),
            ThrottleDecision::Wait { retry_after_ms: 1 }
        );
        assert_eq!(throttle.admit(2_000).unwrap(), ThrottleDecision::Allow);
    }

    #[test]
    fn uncertain_submission_always_reconciles_before_retry() {
        let policy = RetryPolicy::new(4, 100, 1_000).unwrap();
        assert_eq!(
            policy.action(1, SubmissionFailure::UncertainAfterWrite),
            RecoveryAction::ReconcileBeforeRetry
        );
    }

    #[test]
    fn retry_is_bounded_and_respects_longer_venue_delay() {
        let policy = RetryPolicy::new(3, 100, 200).unwrap();
        assert_eq!(
            policy.action(
                1,
                SubmissionFailure::RateLimited {
                    retry_after_ms: Some(750)
                }
            ),
            RecoveryAction::RetryAfter { delay_ms: 750 }
        );
        assert_eq!(
            policy.action(3, SubmissionFailure::TransientBeforeWrite),
            RecoveryAction::Stop
        );
    }

    #[test]
    fn regressive_time_fails_closed() {
        let mut throttle = RequestThrottle::new(1, 1_000).unwrap();
        throttle.admit(2_000).unwrap();
        assert!(throttle.admit(1_999).is_err());
    }
}
