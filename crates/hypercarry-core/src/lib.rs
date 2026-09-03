//! `hypercarry-core` — domain types for Hyperliquid funding, basis, and
//! predicted-funding data.
//!
//! # Milestone status (M4 complete)
//!
//! This crate contains serde models for the Hyperliquid `info` responses, a
//! typed read-only client for those endpoints, and exact funding and basis
//! metrics, and a deterministic next-hour funding predictor. Durable backfill
//! writing lives in `hypercarry-storage`; bounded live capture and replay live
//! in `hypercarry-recorder`. The README roadmap is the source of truth for what
//! is and isn't built.
//!
//! Everything monetary is typed as [`rust_decimal::Decimal`], never `f64`.

#![warn(missing_docs)]

pub mod info;
pub mod metrics;
pub mod predictor;
pub mod types;
