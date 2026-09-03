//! Optional, venue-neutral execution simulation and non-signing dry-run tools.
//!
//! Exchange transport and signer integrations remain default-off. The optional
//! testnet SDK signer accepts a caller-constructed signer; this crate contains
//! no key loader or wallet configuration. The default hypercarry CLI does not
//! depend on it.

#![warn(missing_docs)]

mod dry_run;
mod error;
mod identity;
mod journal;
mod lifecycle;
mod model;
mod network;
mod reliability;
mod risk;
mod signing;
mod simulator;
mod traits;

#[cfg(feature = "testnet-execution")]
mod hyperliquid;

#[cfg(feature = "mainnet-execution")]
mod mainnet;

pub use dry_run::{DryRun, DryRunAdapter};
pub use error::ExecutionError;
pub use identity::ClientOrderId;
pub use journal::{
    FileJournal, InMemoryJournal, Journal, JournalEvent, JournalEventKind, read_journal,
};
pub use lifecycle::{LifecycleTransition, OrderStateMachine, TransitionOutcome};
pub use model::{
    Cancellation, ExecutionMode, ExecutionReport, Fill, MarketMetadata, OrderIntent, OrderState,
    Rejection, Side, ValidatedOrder,
};
pub use network::ExecutionNetwork;
pub use reliability::{
    RecoveryAction, RequestThrottle, RetryPolicy, SubmissionFailure, ThrottleDecision,
};
pub use risk::{
    ComprehensiveRiskPolicy, FileKillSwitch, KillSwitch, RiskLimits, RiskSnapshot,
    RiskSnapshotSource,
};
pub use signing::validate_signer;
pub use simulator::{CancelRacePriority, MarketFrame, SimulationConfig, SimulatorAdapter};
pub use traits::{ExecutionAdapter, MarketMetadataResolver, RiskDecision, RiskPolicy, Signer};

#[cfg(feature = "testnet-execution")]
pub use hyperliquid::{
    Clock, HyperliquidAssetResolver, HyperliquidL1Signer, HyperliquidSigningRequest,
    HyperliquidTestnetConfig, HyperliquidTestnetExecutor, HyperliquidTransport, LiveOrder,
    PrivateEvent, PrivateEventOutcome, PrivateStream, PrivateStreamEvent,
    ReqwestHyperliquidTransport, SystemClock, TESTNET_ACKNOWLEDGEMENT, TestnetAcknowledgement,
    TestnetReliability, TransportFailure, connect_testnet_private_stream, parse_private_message,
    private_subscription_requests,
};

#[cfg(feature = "hypersdk-signer")]
pub use hyperliquid::{HypersdkSignerError, HypersdkTestnetSigner};

#[cfg(feature = "mainnet-execution")]
pub use mainnet::{
    DeadManSwitch, ExplicitMainnetEnable, FileDeadManSwitch, HumanReleaseDecision,
    InteractiveMainnetConfirmation, MAINNET_CONFIRMATION, MainnetAuthorization,
    MainnetReleaseConfig, MainnetReleaseEvidence, MainnetReleaseGate, OperationalHealth,
    OperationalHealthSource, Readiness, ReviewedReleaseBundle, TestnetSessionEvidence,
};

/// Stable schema version for execution values and journal events.
pub const EXECUTION_SCHEMA_VERSION: u32 = 1;
