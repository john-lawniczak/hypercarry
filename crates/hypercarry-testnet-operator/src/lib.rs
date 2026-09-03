//! Testnet-only operator assembly and secret-free evidence support.
//!
//! This crate is intentionally non-shipping. It provides the narrow host that
//! the feature-gated execution library needs for credentialed testnet proof,
//! while keeping key loading and mainnet transport structurally absent.

#![warn(missing_docs)]

mod config;
mod evidence;
mod operator;
mod review;
mod signer;

pub use config::{OperatorConfig, PreflightSummary};
pub use evidence::SessionEvidence;
pub use operator::{OperatorOutcome, preflight, run_session};
pub use review::{
    REVIEW_ACKNOWLEDGEMENT, ReviewAttestation, ReviewOutcome, ReviewRequest, review_session,
};

/// Exact external-effect acknowledgement required by the operator binary.
pub const OPERATOR_ACKNOWLEDGEMENT: &str =
    "I ACKNOWLEDGE HYPERCARRY TESTNET EXECUTION AND EXTERNAL EFFECTS";
