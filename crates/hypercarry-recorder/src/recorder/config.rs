use super::RecorderError;
use hypercarry_core::info::Network;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const DEFAULT_QUEUE_CAPACITY: usize = 1_024;
const DEFAULT_BATCH_SIZE: usize = 256;
const DEFAULT_DUPLICATE_WINDOW: usize = 4_096;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(75);
const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(30);
const DEFAULT_MAX_RECONNECT_ATTEMPTS: u32 = 8;

/// Behavior when normalization cannot keep pace with raw capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackpressurePolicy {
    /// Preserve the complete raw log and discard the newest normalized queue
    /// item. The exact number discarded is reported in diagnostics.
    DropNewest,
}

impl BackpressurePolicy {
    /// Stable lowercase name for diagnostics and tracing.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DropNewest => "drop_newest_normalized",
        }
    }
}

/// Validated recorder behavior and storage destination.
#[derive(Debug, Clone)]
pub struct RecorderConfig {
    pub(super) network: Network,
    pub(super) coins: Vec<String>,
    pub(super) dataset_root: PathBuf,
    pub(super) queue_capacity: usize,
    pub(super) batch_size: usize,
    pub(super) duplicate_window: usize,
    pub(super) connect_timeout: Duration,
    pub(super) heartbeat_interval: Duration,
    pub(super) stale_after: Duration,
    pub(super) initial_backoff: Duration,
    pub(super) max_backoff: Duration,
    pub(super) max_reconnect_attempts: u32,
    pub(super) backpressure_policy: BackpressurePolicy,
}

/// Tunable I/O timing with bounded reconnect behavior.
#[derive(Debug, Clone, Copy)]
pub struct RecorderTiming {
    connect_timeout: Duration,
    heartbeat_interval: Duration,
    stale_after: Duration,
    initial_backoff: Duration,
    max_backoff: Duration,
    max_reconnect_attempts: u32,
}

impl RecorderTiming {
    /// Validate heartbeat, staleness, and reconnect boundaries as one unit.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for zero durations, an ineffective stale
    /// boundary, inverted backoff bounds, or zero reconnect attempts.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        connect_timeout: Duration,
        heartbeat_interval: Duration,
        stale_after: Duration,
        initial_backoff: Duration,
        max_backoff: Duration,
        max_reconnect_attempts: u32,
    ) -> Result<Self, RecorderError> {
        if connect_timeout.is_zero()
            || heartbeat_interval.is_zero()
            || stale_after.is_zero()
            || initial_backoff.is_zero()
            || max_backoff.is_zero()
        {
            return Err(RecorderError::Configuration(
                "recorder timing durations must be positive".to_owned(),
            ));
        }
        if stale_after <= heartbeat_interval {
            return Err(RecorderError::Configuration(
                "stale timeout must exceed the heartbeat interval".to_owned(),
            ));
        }
        if initial_backoff > max_backoff {
            return Err(RecorderError::Configuration(
                "initial reconnect backoff must not exceed its maximum".to_owned(),
            ));
        }
        if max_reconnect_attempts == 0 {
            return Err(RecorderError::Configuration(
                "maximum reconnect attempts must be positive".to_owned(),
            ));
        }
        Ok(Self {
            connect_timeout,
            heartbeat_interval,
            stale_after,
            initial_backoff,
            max_backoff,
            max_reconnect_attempts,
        })
    }
}

impl Default for RecorderTiming {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            stale_after: DEFAULT_STALE_AFTER,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
            max_reconnect_attempts: DEFAULT_MAX_RECONNECT_ATTEMPTS,
        }
    }
}

impl RecorderConfig {
    /// Construct safe production defaults for an explicit network and coin set.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when no non-empty coin is supplied.
    pub fn new(
        network: Network,
        coins: Vec<String>,
        dataset_root: impl Into<PathBuf>,
    ) -> Result<Self, RecorderError> {
        let mut normalized_coins: Vec<_> = coins
            .into_iter()
            .map(|coin| coin.trim().to_owned())
            .collect();
        if normalized_coins.is_empty() || normalized_coins.iter().any(String::is_empty) {
            return Err(RecorderError::Configuration(
                "at least one non-empty coin is required".to_owned(),
            ));
        }
        normalized_coins.sort();
        normalized_coins.dedup();
        Ok(Self {
            network,
            coins: normalized_coins,
            dataset_root: dataset_root.into(),
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            batch_size: DEFAULT_BATCH_SIZE,
            duplicate_window: DEFAULT_DUPLICATE_WINDOW,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            heartbeat_interval: DEFAULT_HEARTBEAT_INTERVAL,
            stale_after: DEFAULT_STALE_AFTER,
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
            max_reconnect_attempts: DEFAULT_MAX_RECONNECT_ATTEMPTS,
            backpressure_policy: BackpressurePolicy::DropNewest,
        })
    }

    /// Override the bounded normalization queue size.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when `capacity` is zero.
    pub fn with_queue_capacity(mut self, capacity: usize) -> Result<Self, RecorderError> {
        if capacity == 0 {
            return Err(RecorderError::Configuration(
                "record queue capacity must be positive".to_owned(),
            ));
        }
        self.queue_capacity = capacity;
        Ok(self)
    }

    /// Override the default connection, heartbeat, and backoff timing.
    #[must_use]
    pub const fn with_timing(mut self, timing: RecorderTiming) -> Self {
        self.connect_timeout = timing.connect_timeout;
        self.heartbeat_interval = timing.heartbeat_interval;
        self.stale_after = timing.stale_after;
        self.initial_backoff = timing.initial_backoff;
        self.max_backoff = timing.max_backoff;
        self.max_reconnect_attempts = timing.max_reconnect_attempts;
        self
    }

    /// Network this recorder subscribes to.
    pub const fn network(&self) -> Network {
        self.network
    }

    /// Perpetual market symbols being recorded.
    pub fn coins(&self) -> &[String] {
        &self.coins
    }

    /// Dataset root where capture and Parquet outputs are written.
    pub fn dataset_root(&self) -> &Path {
        &self.dataset_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_empty_coins_and_unbounded_zero_capacity() {
        assert!(RecorderConfig::new(Network::Testnet, vec![], "data").is_err());
        let config = RecorderConfig::new(Network::Testnet, vec!["BTC".to_owned()], "data")
            .expect("valid config");
        assert!(config.with_queue_capacity(0).is_err());
    }

    #[test]
    fn config_canonicalizes_coin_set_and_timing() {
        let timing = RecorderTiming::new(
            Duration::from_millis(20),
            Duration::from_millis(5),
            Duration::from_millis(10),
            Duration::from_millis(2),
            Duration::from_millis(5),
            2,
        )
        .expect("valid test timing");
        let config = RecorderConfig::new(
            Network::Testnet,
            vec!["ETH".to_owned(), "BTC".to_owned(), "BTC".to_owned()],
            "data",
        )
        .expect("valid config")
        .with_timing(timing);

        assert_eq!(config.coins(), ["BTC", "ETH"]);
        assert_eq!(config.max_backoff, Duration::from_millis(5));
    }
}
