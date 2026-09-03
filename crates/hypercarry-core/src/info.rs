//! Typed client for Hyperliquid's read-only `info` endpoint.
//!
//! Requests encode their response type through [`InfoRequest`]. A caller
//! cannot accidentally decode a funding-history response as asset contexts,
//! and tests can replace the HTTP layer through [`InfoTransport`].

use crate::types::{FundingHistory, MetaAndAssetCtxs, PredictedFundings, TimestampMs};
use reqwest::{
    Client as HttpClient, StatusCode, Url,
    header::{CONTENT_TYPE, HeaderMap, RETRY_AFTER},
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    error::Error,
    fmt,
    future::Future,
    net::IpAddr,
    num::NonZeroU32,
    str::FromStr,
    time::{Duration, Instant, SystemTime},
};

const MAINNET_INFO_ENDPOINT: &str = "https://api.hyperliquid.xyz/info";
const TESTNET_INFO_ENDPOINT: &str = "https://api.hyperliquid-testnet.xyz/info";
const MAINNET_WEBSOCKET_ENDPOINT: &str = "wss://api.hyperliquid.xyz/ws";
const TESTNET_WEBSOCKET_ENDPOINT: &str = "wss://api.hyperliquid-testnet.xyz/ws";
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum number of observations returned by a time-range info request.
pub const FUNDING_HISTORY_PAGE_SIZE: usize = 500;

/// A Hyperliquid deployment with a distinct public API and dataset identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    /// Hyperliquid's production network.
    Mainnet,
    /// Hyperliquid's public test network.
    Testnet,
}

impl Network {
    /// Return the stable lowercase name used in configuration and output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mainnet => "mainnet",
            Self::Testnet => "testnet",
        }
    }

    /// Return the official HTTPS `info` endpoint for this network.
    pub const fn info_endpoint(self) -> &'static str {
        match self {
            Self::Mainnet => MAINNET_INFO_ENDPOINT,
            Self::Testnet => TESTNET_INFO_ENDPOINT,
        }
    }

    /// Return the official public WebSocket endpoint for this network.
    pub const fn websocket_endpoint(self) -> &'static str {
        match self {
            Self::Mainnet => MAINNET_WEBSOCKET_ENDPOINT,
            Self::Testnet => TESTNET_WEBSOCKET_ENDPOINT,
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Network {
    type Err = ParseNetworkError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "mainnet" => Ok(Self::Mainnet),
            "testnet" => Ok(Self::Testnet),
            _ => Err(ParseNetworkError {
                value: value.to_owned(),
            }),
        }
    }
}

/// Error returned when a network name is not `mainnet` or `testnet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseNetworkError {
    value: String,
}

impl fmt::Display for ParseNetworkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown Hyperliquid network {:?}; expected `mainnet` or `testnet`",
            self.value
        )
    }
}

impl Error for ParseNetworkError {}

/// Error returned when a development endpoint violates transport policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevelopmentEndpointError {
    endpoint: Url,
}

impl fmt::Display for DevelopmentEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "development endpoint {} must use HTTPS; plain HTTP is permitted only for localhost or a loopback IP",
            self.endpoint
        )
    }
}

impl Error for DevelopmentEndpointError {}

/// Failure from the production HTTP transport.
///
/// The error retains endpoint and status metadata for diagnostics but never
/// stores a response body.
#[derive(Debug)]
#[non_exhaustive]
pub enum HttpTransportError {
    /// The request failed before an HTTP response was available.
    Request {
        /// Endpoint targeted by the request.
        endpoint: Url,
        /// Underlying reqwest error, including timeout classification.
        source: reqwest::Error,
    },
    /// The server returned a non-success HTTP status.
    Status {
        /// Endpoint targeted by the request.
        endpoint: Url,
        /// HTTP response status.
        status: StatusCode,
        /// Standard `Retry-After` value when the server supplied one.
        retry_after: Option<String>,
    },
    /// The response body could not be read within the request boundary.
    ResponseBody {
        /// Endpoint targeted by the request.
        endpoint: Url,
        /// HTTP status received before body reading failed.
        status: StatusCode,
        /// Underlying reqwest body or timeout error.
        source: reqwest::Error,
    },
}

impl HttpTransportError {
    /// Return the endpoint targeted by the failed request.
    pub const fn endpoint(&self) -> &Url {
        match self {
            Self::Request { endpoint, .. }
            | Self::Status { endpoint, .. }
            | Self::ResponseBody { endpoint, .. } => endpoint,
        }
    }

    /// Return the HTTP status when response headers were received.
    pub const fn status(&self) -> Option<StatusCode> {
        match self {
            Self::Request { .. } => None,
            Self::Status { status, .. } | Self::ResponseBody { status, .. } => Some(*status),
        }
    }

    /// Return the server's standard `Retry-After` value, if present.
    pub fn retry_after(&self) -> Option<&str> {
        match self {
            Self::Status { retry_after, .. } => retry_after.as_deref(),
            Self::Request { .. } | Self::ResponseBody { .. } => None,
        }
    }

    /// Return whether reqwest classified this failure as a timeout.
    pub fn is_timeout(&self) -> bool {
        match self {
            Self::Request { source, .. } | Self::ResponseBody { source, .. } => source.is_timeout(),
            Self::Status { .. } => false,
        }
    }

    /// Return whether the server rejected the request for rate limiting.
    pub const fn is_rate_limited(&self) -> bool {
        matches!(
            self,
            Self::Status {
                status: StatusCode::TOO_MANY_REQUESTS,
                ..
            }
        )
    }
}

impl fmt::Display for HttpTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request { endpoint, source } => {
                write!(formatter, "request to {endpoint} failed: {source}")
            }
            Self::Status {
                endpoint,
                status,
                retry_after,
            } => {
                write!(formatter, "request to {endpoint} returned HTTP {status}")?;
                if let Some(retry_after) = retry_after {
                    write!(formatter, "; retry-after={retry_after}")?;
                }
                Ok(())
            }
            Self::ResponseBody {
                endpoint,
                status,
                source,
            } => write!(
                formatter,
                "failed to read HTTP {status} response body from {endpoint}: {source}"
            ),
        }
    }
}

impl Error for HttpTransportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request { source, .. } | Self::ResponseBody { source, .. } => Some(source),
            Self::Status { .. } => None,
        }
    }
}

/// A boxed transport failure that is safe to pass between threads.
pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// Sends an encoded request to an `info` endpoint and returns its raw response.
///
/// Implement this trait with a fixture transport to test consumers without
/// making network requests.
pub trait InfoTransport {
    /// Return the network identity attached to every response.
    fn network(&self) -> Network;

    /// Send one JSON request body.
    ///
    /// # Errors
    ///
    /// Returns a transport-specific error if the request cannot be completed.
    fn post(&self, request_body: &[u8]) -> impl Future<Output = Result<Vec<u8>, BoxError>> + Send;
}

/// Production asynchronous [`InfoTransport`] backed by reqwest.
#[derive(Debug, Clone)]
pub struct ReqwestInfoTransport {
    client: HttpClient,
    endpoint: Url,
    network: Network,
}

impl ReqwestInfoTransport {
    /// Build the production transport for an explicit Hyperliquid network.
    ///
    /// # Errors
    ///
    /// Returns reqwest's builder error if the HTTP client cannot be created.
    ///
    /// # Panics
    ///
    /// Panics only if an official endpoint constant is not a valid URL, which
    /// indicates a programming error in this crate.
    pub fn for_network(network: Network) -> Result<Self, reqwest::Error> {
        let client = HttpClient::builder()
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .user_agent(concat!("hypercarry/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let endpoint = Url::parse(network.info_endpoint())
            .expect("the hard-coded Hyperliquid info endpoint must be a valid URL");

        Ok(Self {
            client,
            endpoint,
            network,
        })
    }

    /// Build a transport with a custom endpoint for development or testing.
    ///
    /// Plain HTTP is accepted only for `localhost` or a loopback IP. All other
    /// endpoints must use HTTPS. The caller must still attach an explicit
    /// network identity so data from different deployments cannot mix silently.
    ///
    /// # Errors
    ///
    /// Returns [`DevelopmentEndpointError`] unless the endpoint uses HTTPS, or
    /// uses plain HTTP with a localhost or loopback-IP host.
    pub fn for_development(
        network: Network,
        client: HttpClient,
        endpoint: Url,
    ) -> Result<Self, DevelopmentEndpointError> {
        let permits_plain_http = endpoint.scheme() == "http" && is_loopback_endpoint(&endpoint);
        if endpoint.scheme() != "https" && !permits_plain_http {
            return Err(DevelopmentEndpointError { endpoint });
        }

        Ok(Self {
            client,
            endpoint,
            network,
        })
    }

    /// Return the configured endpoint.
    pub const fn endpoint(&self) -> &Url {
        &self.endpoint
    }
}

impl InfoTransport for ReqwestInfoTransport {
    fn network(&self) -> Network {
        self.network
    }

    async fn post(&self, request_body: &[u8]) -> Result<Vec<u8>, BoxError> {
        let started_at = Instant::now();
        let response = self
            .client
            .post(self.endpoint.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(request_body.to_vec())
            .send()
            .await
            .map_err(|source| {
                tracing::warn!(
                    network = %self.network,
                    latency_ms = started_at.elapsed().as_millis(),
                    timeout = source.is_timeout(),
                    "info request failed before response"
                );
                HttpTransportError::Request {
                    endpoint: self.endpoint.clone(),
                    source,
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = retry_after_value(response.headers());
            tracing::warn!(
                network = %self.network,
                status = status.as_u16(),
                latency_ms = started_at.elapsed().as_millis(),
                rate_limited = status == StatusCode::TOO_MANY_REQUESTS,
                retry_after = retry_after.as_deref(),
                "info request returned non-success status"
            );
            return Err(HttpTransportError::Status {
                endpoint: self.endpoint.clone(),
                status,
                retry_after,
            }
            .into());
        }
        let bytes = response.bytes().await.map_err(|source| {
            tracing::warn!(
                network = %self.network,
                status = status.as_u16(),
                latency_ms = started_at.elapsed().as_millis(),
                timeout = source.is_timeout(),
                "failed to read info response body"
            );
            HttpTransportError::ResponseBody {
                endpoint: self.endpoint.clone(),
                status,
                source,
            }
        })?;

        tracing::debug!(
            network = %self.network,
            status = status.as_u16(),
            latency_ms = started_at.elapsed().as_millis(),
            response_bytes = bytes.len(),
            "received info response"
        );
        Ok(bytes.to_vec())
    }
}

fn is_loopback_endpoint(endpoint: &Url) -> bool {
    endpoint.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

fn retry_after_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Error produced while encoding, sending, or decoding an `info` request.
#[derive(Debug)]
#[non_exhaustive]
pub enum InfoClientError {
    /// The typed request could not be encoded as JSON.
    Encode(serde_json::Error),
    /// The transport could not complete the request.
    Transport(BoxError),
    /// The response did not match the response type associated with the request.
    Decode(serde_json::Error),
}

impl InfoClientError {
    /// Return structured production-HTTP context for a transport failure.
    ///
    /// Fixture and other custom transports may return different error types,
    /// in which case this returns `None`.
    pub fn http_transport_error(&self) -> Option<&HttpTransportError> {
        match self {
            Self::Transport(error) => error.downcast_ref(),
            Self::Encode(_) | Self::Decode(_) => None,
        }
    }
}

impl fmt::Display for InfoClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "failed to encode info request: {error}"),
            Self::Transport(error) => write!(formatter, "info request failed: {error}"),
            Self::Decode(error) => write!(formatter, "failed to decode info response: {error}"),
        }
    }
}

impl Error for InfoClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Encode(error) | Self::Decode(error) => Some(error),
            Self::Transport(error) => Some(error.as_ref()),
        }
    }
}

/// Bounded retry behavior for read-only funding-history page requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FundingHistoryRetryPolicy {
    max_attempts: NonZeroU32,
    initial_backoff: Duration,
    max_backoff: Duration,
    max_retry_after: Duration,
}

impl FundingHistoryRetryPolicy {
    /// Build a retry policy. `max_attempts` includes the initial request.
    ///
    /// # Errors
    ///
    /// Returns [`RetryPolicyError`] when `initial_backoff` exceeds
    /// `max_backoff`.
    pub const fn new(
        max_attempts: NonZeroU32,
        initial_backoff: Duration,
        max_backoff: Duration,
        max_retry_after: Duration,
    ) -> Result<Self, RetryPolicyError> {
        if initial_backoff.as_nanos() > max_backoff.as_nanos() {
            return Err(RetryPolicyError {
                initial_backoff,
                max_backoff,
            });
        }
        Ok(Self {
            max_attempts,
            initial_backoff,
            max_backoff,
            max_retry_after,
        })
    }

    /// Maximum total attempts, including the first request.
    pub const fn max_attempts(self) -> NonZeroU32 {
        self.max_attempts
    }

    fn delay_for(
        self,
        error: &InfoClientError,
        failed_attempt: u32,
        entropy: u64,
    ) -> Option<Duration> {
        if failed_attempt >= self.max_attempts.get() {
            return None;
        }
        let http_error = error.http_transport_error()?;
        match http_error {
            HttpTransportError::Request { source, .. }
                if source.is_timeout() || source.is_connect() => {}
            HttpTransportError::ResponseBody { .. } => {}
            HttpTransportError::Status {
                status,
                retry_after,
                ..
            } if *status == StatusCode::TOO_MANY_REQUESTS
                || *status == StatusCode::REQUEST_TIMEOUT
                || status.is_server_error() =>
            {
                if let Some(retry_after) = retry_after {
                    let delay = parse_retry_after_delta_seconds(retry_after)?;
                    return (delay <= self.max_retry_after).then_some(delay);
                }
            }
            HttpTransportError::Request { .. } | HttpTransportError::Status { .. } => return None,
        }

        Some(self.jittered_backoff(failed_attempt, entropy))
    }

    fn jittered_backoff(self, failed_attempt: u32, entropy: u64) -> Duration {
        let exponent = failed_attempt.saturating_sub(1).min(31);
        let multiplier = 1_u32.checked_shl(exponent).unwrap_or(u32::MAX);
        let ceiling = self
            .initial_backoff
            .saturating_mul(multiplier)
            .min(self.max_backoff);
        let ceiling_ms = u64::try_from(ceiling.as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(entropy % ceiling_ms.saturating_add(1))
    }
}

impl Default for FundingHistoryRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: NonZeroU32::new(4).expect("four is non-zero"),
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(5),
            max_retry_after: Duration::from_secs(30),
        }
    }
}

/// Invalid funding-history retry-policy bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicyError {
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl fmt::Display for RetryPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "initial retry backoff {:?} exceeds maximum {:?}",
            self.initial_backoff, self.max_backoff
        )
    }
}

impl Error for RetryPolicyError {}

fn parse_retry_after_delta_seconds(value: &str) -> Option<Duration> {
    value.parse::<u64>().ok().map(Duration::from_secs)
}

fn retry_entropy(page_start: TimestampMs, failed_attempt: u32) -> u64 {
    let clock = SystemTime::UNIX_EPOCH.elapsed().unwrap_or_default();
    let nanos = u64::try_from(clock.as_nanos()).unwrap_or(u64::MAX);
    nanos
        ^ page_start.as_i64().cast_unsigned().rotate_left(17)
        ^ u64::from(failed_attempt).rotate_left(41)
}

/// A bounded funding-history range could not be paginated safely.
#[derive(Debug)]
#[non_exhaustive]
pub enum FundingHistoryPaginationError {
    /// The requested inclusive range has its bounds in reverse order.
    ReversedWindow {
        /// Inclusive requested start time.
        start_ms: i64,
        /// Inclusive requested end time.
        end_ms: i64,
    },
    /// A page request failed before a valid response was available.
    Request(InfoClientError),
    /// The response contained an observation for another coin.
    UnexpectedCoin {
        /// Requested perpetual symbol.
        expected: String,
        /// Symbol returned by the venue.
        actual: String,
        /// Settlement carrying the unexpected symbol.
        settlement_time_ms: i64,
    },
    /// The response contained an observation outside the page it answered.
    ObservationOutsidePage {
        /// Out-of-range settlement time.
        settlement_time_ms: i64,
        /// Inclusive start of the page request.
        page_start_ms: i64,
        /// Inclusive end of the complete requested range.
        end_ms: i64,
    },
    /// Two rows had the same identity timestamp but different values.
    ConflictingObservation {
        /// Settlement identity shared by the conflicting rows.
        settlement_time_ms: i64,
    },
    /// A full page did not move beyond its inclusive starting timestamp.
    Stalled {
        /// Inclusive start requested for the stalled page.
        start_ms: i64,
        /// Latest settlement returned by the stalled page.
        last_ms: i64,
    },
}

impl fmt::Display for FundingHistoryPaginationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReversedWindow { start_ms, end_ms } => write!(
                formatter,
                "funding-history range starts at {start_ms}ms after it ends at {end_ms}ms"
            ),
            Self::Request(error) => {
                write!(formatter, "funding-history page request failed: {error}")
            }
            Self::UnexpectedCoin {
                expected,
                actual,
                settlement_time_ms,
            } => write!(
                formatter,
                "funding-history response for {expected:?} contained coin {actual:?} at {settlement_time_ms}ms"
            ),
            Self::ObservationOutsidePage {
                settlement_time_ms,
                page_start_ms,
                end_ms,
            } => write!(
                formatter,
                "funding-history observation {settlement_time_ms}ms is outside requested page {page_start_ms}..={end_ms}ms"
            ),
            Self::ConflictingObservation { settlement_time_ms } => write!(
                formatter,
                "funding-history response contained conflicting rows at {settlement_time_ms}ms"
            ),
            Self::Stalled { start_ms, last_ms } => write!(
                formatter,
                "funding-history pagination stalled at inclusive start {start_ms}ms (last row {last_ms}ms)"
            ),
        }
    }
}

impl Error for FundingHistoryPaginationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request(error) => Some(error),
            Self::ReversedWindow { .. }
            | Self::UnexpectedCoin { .. }
            | Self::ObservationOutsidePage { .. }
            | Self::ConflictingObservation { .. }
            | Self::Stalled { .. } => None,
        }
    }
}

/// A typed `info` client using an injectable transport.
#[derive(Debug, Clone)]
pub struct InfoClient<T> {
    transport: T,
}

impl<T> InfoClient<T> {
    /// Construct a client around an HTTP or fixture transport.
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Return a shared reference to the underlying transport.
    pub const fn transport(&self) -> &T {
        &self.transport
    }
}

impl<T: InfoTransport> InfoClient<T> {
    /// Return the network identity attached to this client's responses.
    pub fn network(&self) -> Network {
        self.transport.network()
    }

    /// Execute a request and decode its statically associated response type.
    ///
    /// # Errors
    ///
    /// Returns [`InfoClientError`] when request encoding, transport, or response
    /// decoding fails.
    #[tracing::instrument(
        level = "debug",
        skip_all,
        fields(request_type = R::TYPE, network = %self.network())
    )]
    pub async fn execute<R: InfoRequest>(
        &self,
        request: &R,
    ) -> Result<R::Response, InfoClientError> {
        let body = request.encode().map_err(InfoClientError::Encode)?;
        let response = self
            .transport
            .post(&body)
            .await
            .map_err(InfoClientError::Transport)?;

        serde_json::from_slice(&response).map_err(InfoClientError::Decode)
    }

    /// Fetch an inclusive funding-history range across all API pages.
    ///
    /// Hyperliquid caps time-range responses at 500 elements and documents the
    /// last returned timestamp as the next inclusive `startTime`. This method
    /// removes the repeated boundary row, rejects conflicting duplicates, and
    /// returns rows in ascending settlement-time order.
    ///
    /// # Errors
    ///
    /// Returns [`FundingHistoryPaginationError`] for invalid bounds, request
    /// failures, malformed page semantics, or a full page that cannot advance.
    #[tracing::instrument(
        level = "debug",
        skip(self),
        fields(network = %self.network(), start_ms = start_time.as_i64(), end_ms = end_time.as_i64())
    )]
    pub async fn funding_history_range(
        &self,
        coin: &str,
        start_time: TimestampMs,
        end_time: TimestampMs,
    ) -> Result<FundingHistory, FundingHistoryPaginationError> {
        self.funding_history_range_with_retry(
            coin,
            start_time,
            end_time,
            FundingHistoryRetryPolicy::default(),
        )
        .await
    }

    /// Fetch an inclusive funding-history range with an explicit retry policy.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::funding_history_range`]. Only
    /// transient transport errors, HTTP 408/429, and HTTP 5xx responses are
    /// retried; encoding and response-schema errors fail immediately.
    #[tracing::instrument(
        level = "debug",
        skip(self),
        fields(network = %self.network(), start_ms = start_time.as_i64(), end_ms = end_time.as_i64())
    )]
    pub async fn funding_history_range_with_retry(
        &self,
        coin: &str,
        start_time: TimestampMs,
        end_time: TimestampMs,
        retry_policy: FundingHistoryRetryPolicy,
    ) -> Result<FundingHistory, FundingHistoryPaginationError> {
        if start_time > end_time {
            return Err(FundingHistoryPaginationError::ReversedWindow {
                start_ms: start_time.as_i64(),
                end_ms: end_time.as_i64(),
            });
        }

        let mut page_start = start_time;
        let mut observations = BTreeMap::new();

        loop {
            let request = FundingHistoryRequest::new(coin, page_start).with_end_time(end_time);
            let mut page = self
                .execute_funding_history_page(&request, retry_policy)
                .await
                .map_err(FundingHistoryPaginationError::Request)?;
            let page_len = page.len();
            if page_len == 0 {
                break;
            }
            page.sort_by_key(|entry| entry.time);
            let Some(last_time) = page.last().map(|entry| entry.time) else {
                break;
            };

            tracing::debug!(
                coin,
                page_start_ms = page_start.as_i64(),
                end_ms = end_time.as_i64(),
                returned = page_len,
                last_ms = last_time.as_i64(),
                "received funding-history page"
            );

            for observation in page {
                if observation.coin != coin {
                    return Err(FundingHistoryPaginationError::UnexpectedCoin {
                        expected: coin.to_owned(),
                        actual: observation.coin,
                        settlement_time_ms: observation.time.as_i64(),
                    });
                }
                if observation.time < page_start || observation.time > end_time {
                    return Err(FundingHistoryPaginationError::ObservationOutsidePage {
                        settlement_time_ms: observation.time.as_i64(),
                        page_start_ms: page_start.as_i64(),
                        end_ms: end_time.as_i64(),
                    });
                }

                match observations.entry(observation.time.as_i64()) {
                    Entry::Vacant(entry) => {
                        entry.insert(observation);
                    }
                    Entry::Occupied(entry) if entry.get() == &observation => {}
                    Entry::Occupied(_) => {
                        return Err(FundingHistoryPaginationError::ConflictingObservation {
                            settlement_time_ms: observation.time.as_i64(),
                        });
                    }
                }
            }

            if page_len < FUNDING_HISTORY_PAGE_SIZE || last_time >= end_time {
                break;
            }
            if last_time <= page_start {
                return Err(FundingHistoryPaginationError::Stalled {
                    start_ms: page_start.as_i64(),
                    last_ms: last_time.as_i64(),
                });
            }
            page_start = last_time;
        }

        Ok(observations.into_values().collect())
    }

    async fn execute_funding_history_page(
        &self,
        request: &FundingHistoryRequest,
        retry_policy: FundingHistoryRetryPolicy,
    ) -> Result<FundingHistory, InfoClientError> {
        let mut attempt = 1_u32;
        loop {
            match self.execute(request).await {
                Ok(page) => return Ok(page),
                Err(error) => {
                    let Some(delay) = retry_policy.delay_for(
                        &error,
                        attempt,
                        retry_entropy(request.start_time, attempt),
                    ) else {
                        return Err(error);
                    };
                    let http_error = error.http_transport_error();
                    tracing::warn!(
                        network = %self.network(),
                        coin = request.coin,
                        page_start_ms = request.start_time.as_i64(),
                        attempt,
                        max_attempts = retry_policy.max_attempts().get(),
                        delay_ms = delay.as_millis(),
                        status = http_error.and_then(HttpTransportError::status).map(|status| status.as_u16()),
                        rate_limited = http_error.is_some_and(HttpTransportError::is_rate_limited),
                        "retrying transient funding-history page failure"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }
}

/// A request whose concrete type determines its decoded response type.
///
/// The trait is sealed because Hyperliquid controls the accepted request
/// shapes. Construct one of this module's request types instead.
pub trait InfoRequest: private::Sealed {
    /// Response returned for this request.
    type Response: DeserializeOwned;

    /// Hyperliquid's request discriminator.
    const TYPE: &'static str;

    #[doc(hidden)]
    fn encode(&self) -> Result<Vec<u8>, serde_json::Error>;
}

/// Request settled funding observations for one coin.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FundingHistoryRequest {
    /// Hyperliquid coin symbol, such as `BTC`.
    pub coin: String,
    /// Inclusive beginning of the requested window.
    pub start_time: TimestampMs,
    /// Optional inclusive end of the requested window.
    pub end_time: Option<TimestampMs>,
}

impl FundingHistoryRequest {
    /// Request history from `start_time` onward.
    pub fn new(coin: impl Into<String>, start_time: TimestampMs) -> Self {
        Self {
            coin: coin.into(),
            start_time,
            end_time: None,
        }
    }

    /// Bound the requested history window at `end_time`.
    #[must_use]
    pub const fn with_end_time(mut self, end_time: TimestampMs) -> Self {
        self.end_time = Some(end_time);
        self
    }
}

impl InfoRequest for FundingHistoryRequest {
    type Response = FundingHistory;

    const TYPE: &'static str = "fundingHistory";

    fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            #[serde(rename = "type")]
            request_type: &'static str,
            coin: &'a str,
            start_time: TimestampMs,
            #[serde(skip_serializing_if = "Option::is_none")]
            end_time: Option<TimestampMs>,
        }

        serde_json::to_vec(&Body {
            request_type: Self::TYPE,
            coin: &self.coin,
            start_time: self.start_time,
            end_time: self.end_time,
        })
    }
}

/// Request the perp universe metadata and its parallel asset-context array.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetaAndAssetCtxsRequest;

impl InfoRequest for MetaAndAssetCtxsRequest {
    type Response = MetaAndAssetCtxs;

    const TYPE: &'static str = "metaAndAssetCtxs";

    fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        encode_type_only(Self::TYPE)
    }
}

/// Request predicted funding rates across supported venues.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PredictedFundingsRequest;

impl InfoRequest for PredictedFundingsRequest {
    type Response = PredictedFundings;

    const TYPE: &'static str = "predictedFundings";

    fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        encode_type_only(Self::TYPE)
    }
}

fn encode_type_only(request_type: &'static str) -> Result<Vec<u8>, serde_json::Error> {
    #[derive(Serialize)]
    struct Body {
        #[serde(rename = "type")]
        request_type: &'static str,
    }

    serde_json::to_vec(&Body { request_type })
}

mod private {
    pub trait Sealed {}

    impl Sealed for super::FundingHistoryRequest {}
    impl Sealed for super::MetaAndAssetCtxsRequest {}
    impl Sealed for super::PredictedFundingsRequest {}
}

#[cfg(test)]
mod tests {
    use super::{
        FundingHistoryRetryPolicy, HttpTransportError, InfoClientError, RetryPolicyError,
        retry_after_value,
    };
    use reqwest::{
        StatusCode, Url,
        header::{HeaderMap, HeaderValue, RETRY_AFTER},
    };
    use std::{num::NonZeroU32, time::Duration};

    fn status_error(status: StatusCode, retry_after: Option<&str>) -> InfoClientError {
        InfoClientError::Transport(Box::new(HttpTransportError::Status {
            endpoint: Url::parse("https://example.test/info").expect("test URL parses"),
            status,
            retry_after: retry_after.map(str::to_owned),
        }))
    }

    #[test]
    fn retry_after_is_optional_and_preserved_verbatim() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after_value(&headers), None);

        headers.insert(RETRY_AFTER, HeaderValue::from_static("7"));
        assert_eq!(retry_after_value(&headers).as_deref(), Some("7"));
    }

    #[test]
    fn retry_policy_applies_full_jitter_exponential_backoff_and_cap() {
        let policy = FundingHistoryRetryPolicy::new(
            NonZeroU32::new(4).expect("non-zero attempts"),
            Duration::from_millis(100),
            Duration::from_millis(250),
            Duration::from_secs(30),
        )
        .expect("valid policy");
        let error = status_error(StatusCode::SERVICE_UNAVAILABLE, None);

        assert_eq!(policy.delay_for(&error, 1, 0), Some(Duration::ZERO));
        assert_eq!(
            policy.delay_for(&error, 1, 100),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            policy.delay_for(&error, 2, 200),
            Some(Duration::from_millis(200))
        );
        assert_eq!(
            policy.delay_for(&error, 3, 250),
            Some(Duration::from_millis(250))
        );
        assert_eq!(policy.delay_for(&error, 4, 0), None);
    }

    #[test]
    fn retry_policy_honors_bounded_delta_seconds_and_rejects_unsafe_headers() {
        let policy = FundingHistoryRetryPolicy::default();

        assert_eq!(
            policy.delay_for(
                &status_error(StatusCode::TOO_MANY_REQUESTS, Some("7")),
                1,
                0
            ),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            policy.delay_for(
                &status_error(StatusCode::TOO_MANY_REQUESTS, Some("31")),
                1,
                0
            ),
            None
        );
        assert_eq!(
            policy.delay_for(
                &status_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    Some("Wed, 21 Oct 2015 07:28:00 GMT")
                ),
                1,
                0
            ),
            None
        );
        assert_eq!(
            policy.delay_for(&status_error(StatusCode::BAD_REQUEST, None), 1, 0),
            None
        );
    }

    #[test]
    fn retry_policy_rejects_inverted_backoff_bounds() {
        assert_eq!(
            FundingHistoryRetryPolicy::new(
                NonZeroU32::MIN,
                Duration::from_secs(2),
                Duration::from_secs(1),
                Duration::from_secs(30),
            ),
            Err(RetryPolicyError {
                initial_backoff: Duration::from_secs(2),
                max_backoff: Duration::from_secs(1),
            })
        );
    }
}
