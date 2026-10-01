//! Hyperliquid-specific wire mapping, isolated signing and lifecycle adapters.
//! Mainnet order submission requires a consumed release authorization.
#![warn(missing_docs)]

#[cfg(any(feature = "testnet-execution", feature = "mainnet-execution"))]
mod venue;
#[cfg(feature = "mainnet-execution")]
pub use venue::mainnet::{
    HyperliquidMainnetConfig, HyperliquidMainnetExecutor, connect_mainnet_private_stream,
};
#[cfg(any(feature = "testnet-execution", feature = "mainnet-execution"))]
pub use venue::{
    Clock, HyperliquidAssetResolver, HyperliquidL1Signer, HyperliquidSigningRequest, LiveOrder,
    PrivateEvent, PrivateEventOutcome, PrivateStream, PrivateStreamEvent,
    SCHEDULE_CANCEL_MAX_TRIGGERS_PER_DAY, SystemClock, parse_private_message,
    private_subscription_requests,
};
#[cfg(feature = "testnet-execution")]
pub use venue::{
    HyperliquidTestnetConfig, HyperliquidTestnetExecutor, HyperliquidTransport,
    ReqwestHyperliquidTransport, TESTNET_ACKNOWLEDGEMENT, TestnetAcknowledgement,
    TestnetReliability, TransportFailure, connect_testnet_private_stream,
};
#[cfg(all(feature = "hypersdk-signer", feature = "testnet-execution"))]
pub use venue::{HypersdkSignerError, HypersdkTestnetSigner};
