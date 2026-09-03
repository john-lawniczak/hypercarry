//! Versioned storage contracts for hypercarry datasets.
//!
//! It defines schemas and validated rows together with deterministic Parquet
//! commits and atomic resume checkpoints. That keeps backfills, readers, and
//! replay tools aligned on one durable representation.

#![warn(missing_docs)]

pub mod dataset;
pub mod settled_funding;
