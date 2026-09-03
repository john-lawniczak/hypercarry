use crate::{
    ExecutionError, ExecutionMode, ExecutionNetwork, ExecutionReport, MarketMetadata,
    ValidatedOrder,
};

/// Resolves venue rules without coupling strategies to an exchange SDK.
pub trait MarketMetadataResolver {
    /// Returns the exact metadata for a venue and market.
    ///
    /// # Errors
    ///
    /// Returns an error when metadata is unavailable, stale, or invalid.
    fn resolve(&self, venue: &str, market: &str) -> Result<MarketMetadata, ExecutionError>;
}

/// Deterministic policy result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskDecision {
    /// The order is allowed.
    Allow {
        /// Stable decision code.
        code: String,
        /// Human-readable decision reason.
        reason: String,
    },
    /// The order is rejected.
    Reject {
        /// Stable decision code.
        code: String,
        /// Human-readable decision reason.
        reason: String,
    },
}

/// Fail-closed policy boundary evaluated after metadata resolution.
pub trait RiskPolicy {
    /// Makes a deterministic, fail-closed decision for a validated order.
    ///
    /// # Errors
    ///
    /// Returns an error when the policy cannot make a reliable decision.
    fn evaluate(&self, order: &ValidatedOrder) -> Result<RiskDecision, ExecutionError>;

    /// Re-checks only the process-independent kill switch, for gating the
    /// irreversible network submission immediately before it happens.
    ///
    /// `evaluate` reads the kill switch once, but signing can take an
    /// unbounded amount of wall-clock time (an interactive external signer,
    /// a remote HSM round-trip). A caller that wants an emergency stop to
    /// bind an in-flight, not-yet-sent order must re-check right before the
    /// point of no return. Policies without a kill switch keep the default
    /// "not engaged" answer.
    ///
    /// # Errors
    ///
    /// Returns an error when kill-switch state cannot be read.
    fn kill_switch_engaged(&self) -> Result<bool, ExecutionError> {
        Ok(false)
    }
}

/// Shared validated-order boundary for simulation, dry-run, and future adapters.
pub trait ExecutionAdapter {
    /// Mode this adapter reports on its execution results.
    fn mode(&self) -> ExecutionMode;

    /// Processes an already validated order.
    ///
    /// # Errors
    ///
    /// Returns an error when the adapter cannot produce a definitive report.
    fn execute(&mut self, order: &ValidatedOrder) -> Result<ExecutionReport, ExecutionError>;
}

/// Isolated signing-provider boundary.
///
/// Providers select one key outside hypercarry configuration and expose only a
/// non-secret audit identifier. Dry-run and simulation never accept a signer,
/// making accidental invocation structurally impossible.
pub trait Signer {
    /// Error type returned by the signing provider.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Network for which the selected provider key is authorized.
    fn network(&self) -> ExecutionNetwork;

    /// Non-secret provider/key alias suitable for audit logs.
    fn key_id(&self) -> &str;

    /// Signs an opaque provider-specific payload.
    ///
    /// # Errors
    ///
    /// Returns the provider's error without exposing secret material.
    fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, Self::Error>;
}
