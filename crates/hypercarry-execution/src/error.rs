use std::{error::Error, fmt, io};

/// Validation, simulation, policy, or durable-journal failure.
#[derive(Debug)]
pub enum ExecutionError {
    /// An order or value failed validation.
    Validation(String),
    /// Venue metadata was missing or invalid.
    Metadata(String),
    /// A risk policy rejected the order.
    Policy(String),
    /// An illegal lifecycle transition was attempted.
    Lifecycle(String),
    /// A throttle or retry constraint was violated.
    Reliability(String),
    /// The exchange transport failed.
    Transport(String),
    /// A mainnet release-gate precondition was not met.
    ReleaseGate(String),
    /// The deterministic simulator produced an error.
    Simulation(String),
    /// Writing the durable journal failed.
    JournalIo(io::Error),
    /// A journal record used an unsupported schema.
    JournalSchema(String),
    /// Encoding or decoding a journal record failed.
    JournalJson(serde_json::Error),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) => write!(formatter, "invalid order: {message}"),
            Self::Metadata(message) => write!(formatter, "invalid market metadata: {message}"),
            Self::Policy(message) => write!(formatter, "risk policy failed: {message}"),
            Self::Lifecycle(message) => write!(formatter, "order lifecycle failed: {message}"),
            Self::Reliability(message) => {
                write!(formatter, "submission reliability failed: {message}")
            }
            Self::Transport(message) => write!(formatter, "execution transport failed: {message}"),
            Self::ReleaseGate(message) => {
                write!(formatter, "mainnet release gate failed: {message}")
            }
            Self::Simulation(message) => write!(formatter, "simulation failed: {message}"),
            Self::JournalIo(error) => write!(formatter, "execution journal I/O failed: {error}"),
            Self::JournalSchema(message) => {
                write!(formatter, "execution journal schema failed: {message}")
            }
            Self::JournalJson(error) => write!(formatter, "execution journal JSON failed: {error}"),
        }
    }
}

impl Error for ExecutionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::JournalIo(error) => Some(error),
            Self::JournalJson(error) => Some(error),
            Self::Validation(_)
            | Self::Metadata(_)
            | Self::Policy(_)
            | Self::Lifecycle(_)
            | Self::Reliability(_)
            | Self::Transport(_)
            | Self::ReleaseGate(_)
            | Self::Simulation(_)
            | Self::JournalSchema(_) => None,
        }
    }
}

impl From<io::Error> for ExecutionError {
    fn from(error: io::Error) -> Self {
        Self::JournalIo(error)
    }
}

impl From<serde_json::Error> for ExecutionError {
    fn from(error: serde_json::Error) -> Self {
        Self::JournalJson(error)
    }
}
