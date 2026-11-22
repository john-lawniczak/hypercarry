//! Durable local notifications. The default library has no HTTP or signing dependency.
//! Delivery adapters are opt-in and run outside trading/recovery processes.
pub mod model;
#[cfg(feature = "delivery")]
pub mod providers;
pub mod store;
pub use model::*;
pub use store::*;
