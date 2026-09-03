use crate::{normalized::NormalizeError, raw::RawCaptureError, transport::TransportError};
use std::{error::Error, fmt, time::SystemTimeError};

/// Recorder configuration, storage, or runtime failure.
#[derive(Debug)]
pub enum RecorderError {
    /// The recorder configuration was invalid.
    Configuration(String),
    /// Reading the system clock failed.
    Clock(SystemTimeError),
    /// A recorder filesystem operation failed.
    Io(std::io::Error),
    /// Writing the raw capture failed.
    Raw(RawCaptureError),
    /// Normalizing a frame or writing Parquet failed.
    Normalize(NormalizeError),
    /// The market-data transport failed.
    Transport(TransportError),
    /// Reconnection attempts were exhausted without recovery.
    ReconnectExhausted {
        /// Reconnection attempts made before giving up.
        attempts: u32,
        /// Last transport error observed.
        last_error: String,
    },
    /// A background recorder task failed or panicked.
    Task(String),
}

impl fmt::Display for RecorderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) => formatter.write_str(message),
            Self::Clock(error) => write!(formatter, "could not read system clock: {error}"),
            Self::Io(error) => write!(formatter, "recorder filesystem operation failed: {error}"),
            Self::Raw(error) => error.fmt(formatter),
            Self::Normalize(error) => error.fmt(formatter),
            Self::Transport(error) => error.fmt(formatter),
            Self::ReconnectExhausted {
                attempts,
                last_error,
            } => write!(
                formatter,
                "market WebSocket reconnect exhausted after {attempts} consecutive failures: {last_error}"
            ),
            Self::Task(message) => write!(formatter, "recorder task failed: {message}"),
        }
    }
}

impl Error for RecorderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Clock(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Raw(error) => Some(error),
            Self::Normalize(error) => Some(error),
            Self::Transport(error) => Some(error),
            Self::Configuration(_) | Self::ReconnectExhausted { .. } | Self::Task(_) => None,
        }
    }
}

impl From<RawCaptureError> for RecorderError {
    fn from(value: RawCaptureError) -> Self {
        Self::Raw(value)
    }
}

impl From<NormalizeError> for RecorderError {
    fn from(value: NormalizeError) -> Self {
        Self::Normalize(value)
    }
}

impl From<TransportError> for RecorderError {
    fn from(value: TransportError) -> Self {
        Self::Transport(value)
    }
}
