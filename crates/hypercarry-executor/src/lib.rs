//! Shared configuration, health checks and external signer client.
#![cfg(feature = "mainnet-execution")]
pub mod approval;
pub mod config;
pub mod lease;
pub mod safety;
pub mod signer;
