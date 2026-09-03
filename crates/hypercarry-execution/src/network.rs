use serde::{Deserialize, Serialize};
use std::fmt;

/// Typed execution network. Network selection is never inferred from a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionNetwork {
    /// Hyperliquid testnet.
    Testnet,
    /// Hyperliquid mainnet.
    Mainnet,
}

impl ExecutionNetwork {
    /// Stable lower-case configuration and audit label.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Testnet => "testnet",
            Self::Mainnet => "mainnet",
        }
    }
}

impl fmt::Display for ExecutionNetwork {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
