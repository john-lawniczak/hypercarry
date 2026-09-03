//! The `info` client is tested entirely against recorded responses. The fake
//! transport also validates the exact request JSON sent by each typed request.

use hypercarry_core::{
    info::{
        BoxError, FUNDING_HISTORY_PAGE_SIZE, FundingHistoryPaginationError, FundingHistoryRequest,
        FundingHistoryRetryPolicy, HttpTransportError, InfoClient, InfoClientError, InfoTransport,
        MetaAndAssetCtxsRequest, Network, PredictedFundingsRequest, ReqwestInfoTransport,
    },
    types::{FundingHistory, MetaAndAssetCtxs, PredictedFundings, TimestampMs},
};
use reqwest::Url;
use serde_json::Value;
use std::{
    collections::VecDeque,
    future::Future,
    io,
    num::NonZeroU32,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds")
        .block_on(future)
}

#[derive(Debug)]
struct FixtureTransport {
    requests: Mutex<Vec<Value>>,
}

impl FixtureTransport {
    fn new() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl InfoTransport for FixtureTransport {
    fn network(&self) -> Network {
        Network::Testnet
    }

    #[allow(clippy::unused_async_trait_impl)]
    async fn post(&self, request_body: &[u8]) -> Result<Vec<u8>, BoxError> {
        let body: Value = serde_json::from_slice(request_body)?;
        let response = match body.get("type").and_then(Value::as_str) {
            Some("fundingHistory") => include_bytes!("fixtures/funding_history.json").as_slice(),
            Some("metaAndAssetCtxs") => {
                include_bytes!("fixtures/meta_and_asset_ctxs.json").as_slice()
            }
            Some("predictedFundings") => {
                include_bytes!("fixtures/predicted_fundings.json").as_slice()
            }
            request_type => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unexpected request type: {request_type:?}"),
                )
                .into());
            }
        };

        self.requests
            .lock()
            .expect("request lock poisoned")
            .push(body);
        Ok(response.to_vec())
    }
}

#[test]
fn request_types_decode_only_their_associated_response() {
    let client = InfoClient::new(FixtureTransport::new());

    let history: FundingHistory = block_on(
        client.execute(
            &FundingHistoryRequest::new("BTC", TimestampMs::new(1_683_849_600_048))
                .with_end_time(TimestampMs::new(1_683_856_800_048)),
        ),
    )
    .expect("funding history fixture decodes");
    let contexts: MetaAndAssetCtxs =
        block_on(client.execute(&MetaAndAssetCtxsRequest)).expect("asset contexts fixture decodes");
    let snapshots = contexts.into_snapshots().expect("fixture arrays align");
    let predictions: PredictedFundings = block_on(client.execute(&PredictedFundingsRequest))
        .expect("predicted funding fixture decodes");

    assert_eq!(client.network(), Network::Testnet);
    assert_eq!(history.len(), 3);
    assert_eq!(snapshots[0].meta.name, "BTC");
    assert_eq!(predictions[0].0, "BTC");

    let requests = client
        .transport()
        .requests
        .lock()
        .expect("request lock poisoned");
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["type"], "fundingHistory");
    assert_eq!(requests[0]["coin"], "BTC");
    assert_eq!(requests[0]["startTime"], 1_683_849_600_048_i64);
    assert_eq!(requests[0]["endTime"], 1_683_856_800_048_i64);
    assert_eq!(
        requests[1],
        serde_json::json!({ "type": "metaAndAssetCtxs" })
    );
    assert_eq!(
        requests[2],
        serde_json::json!({ "type": "predictedFundings" })
    );
}

#[derive(Debug)]
struct PaginationTransport {
    pages: Mutex<VecDeque<Vec<u8>>>,
    requests: Mutex<Vec<Value>>,
}

impl PaginationTransport {
    fn new(pages: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            pages: Mutex::new(pages.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl InfoTransport for PaginationTransport {
    fn network(&self) -> Network {
        Network::Testnet
    }

    #[allow(clippy::unused_async_trait_impl)]
    async fn post(&self, request_body: &[u8]) -> Result<Vec<u8>, BoxError> {
        let request: Value = serde_json::from_slice(request_body)?;
        self.requests
            .lock()
            .expect("request lock poisoned")
            .push(request);
        self.pages
            .lock()
            .expect("page lock poisoned")
            .pop_front()
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "no fixture page remains").into()
            })
    }
}

fn funding_page(times: impl IntoIterator<Item = i64>) -> Vec<u8> {
    let rows: Vec<_> = times
        .into_iter()
        .map(|time| {
            serde_json::json!({
                "coin": "BTC",
                "fundingRate": "0.0000125",
                "premium": "0.00000625",
                "time": time,
            })
        })
        .collect();
    serde_json::to_vec(&rows).expect("fixture page serializes")
}

#[test]
fn funding_history_pagination_deduplicates_the_inclusive_boundary_without_gaps() {
    let first_page_start = 1_000_i64;
    let first_page_end = first_page_start
        + i64::try_from(FUNDING_HISTORY_PAGE_SIZE).expect("page size fits i64")
        - 1;
    let end = first_page_end + 2;
    let client = InfoClient::new(PaginationTransport::new([
        funding_page(first_page_start..=first_page_end),
        funding_page([first_page_end, first_page_end + 1, end]),
    ]));

    let history = block_on(client.funding_history_range(
        "BTC",
        TimestampMs::new(first_page_start),
        TimestampMs::new(end),
    ))
    .expect("inclusive pages join");

    assert_eq!(history.len(), FUNDING_HISTORY_PAGE_SIZE + 2);
    assert_eq!(history.first().expect("first row").time.as_i64(), 1_000);
    assert_eq!(history.last().expect("last row").time.as_i64(), end);
    assert!(
        history
            .windows(2)
            .all(|pair| pair[1].time.as_i64() - pair[0].time.as_i64() == 1)
    );

    let requests = client
        .transport()
        .requests
        .lock()
        .expect("request lock poisoned");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["startTime"], first_page_start);
    assert_eq!(requests[1]["startTime"], first_page_end);
    assert_eq!(requests[0]["endTime"], end);
    assert_eq!(requests[1]["endTime"], end);
}

#[test]
fn funding_history_pagination_rejects_a_full_page_that_cannot_advance() {
    let start = 1_000_i64;
    let client = InfoClient::new(PaginationTransport::new([funding_page(
        std::iter::repeat_n(start, FUNDING_HISTORY_PAGE_SIZE),
    )]));

    let error = block_on(client.funding_history_range(
        "BTC",
        TimestampMs::new(start),
        TimestampMs::new(2_000),
    ))
    .expect_err("a repeated full page must not loop forever");

    assert!(matches!(
        error,
        FundingHistoryPaginationError::Stalled {
            start_ms: 1_000,
            last_ms: 1_000,
        }
    ));
}

#[derive(Debug)]
enum RetryResponse {
    Status {
        status: reqwest::StatusCode,
        retry_after: Option<&'static str>,
    },
    Body(Vec<u8>),
}

#[derive(Debug)]
struct RetryTransport {
    responses: Mutex<VecDeque<RetryResponse>>,
    requests: Mutex<Vec<Value>>,
}

impl RetryTransport {
    fn new(responses: impl IntoIterator<Item = RetryResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("request lock poisoned").len()
    }
}

impl InfoTransport for RetryTransport {
    fn network(&self) -> Network {
        Network::Testnet
    }

    #[allow(clippy::unused_async_trait_impl)]
    async fn post(&self, request_body: &[u8]) -> Result<Vec<u8>, BoxError> {
        self.requests
            .lock()
            .expect("request lock poisoned")
            .push(serde_json::from_slice(request_body)?);
        match self
            .responses
            .lock()
            .expect("response lock poisoned")
            .pop_front()
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "no response remains"))?
        {
            RetryResponse::Status {
                status,
                retry_after,
            } => Err(HttpTransportError::Status {
                endpoint: Url::parse(Network::Testnet.info_endpoint())
                    .expect("official endpoint parses"),
                status,
                retry_after: retry_after.map(str::to_owned),
            }
            .into()),
            RetryResponse::Body(body) => Ok(body),
        }
    }
}

fn immediate_retry_policy(max_attempts: u32) -> FundingHistoryRetryPolicy {
    FundingHistoryRetryPolicy::new(
        NonZeroU32::new(max_attempts).expect("test attempts are non-zero"),
        Duration::ZERO,
        Duration::ZERO,
        Duration::ZERO,
    )
    .expect("zero-delay policy is valid")
}

#[test]
fn funding_history_retry_honors_rate_limit_then_recovers() {
    let client = InfoClient::new(RetryTransport::new([
        RetryResponse::Status {
            status: reqwest::StatusCode::TOO_MANY_REQUESTS,
            retry_after: Some("0"),
        },
        RetryResponse::Body(funding_page([1_000])),
    ]));

    let history = block_on(client.funding_history_range_with_retry(
        "BTC",
        TimestampMs::new(1_000),
        TimestampMs::new(1_000),
        immediate_retry_policy(3),
    ))
    .expect("rate-limited page recovers");

    assert_eq!(history.len(), 1);
    assert_eq!(client.transport().request_count(), 2);
}

#[test]
fn funding_history_retry_stops_at_the_attempt_limit() {
    let client = InfoClient::new(RetryTransport::new((0..3).map(|_| RetryResponse::Status {
        status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
        retry_after: None,
    })));

    let error = block_on(client.funding_history_range_with_retry(
        "BTC",
        TimestampMs::new(1_000),
        TimestampMs::new(2_000),
        immediate_retry_policy(3),
    ))
    .expect_err("transient failures must remain bounded");

    assert!(matches!(
        error,
        FundingHistoryPaginationError::Request(InfoClientError::Transport(_))
    ));
    assert_eq!(client.transport().request_count(), 3);
}

#[test]
fn funding_history_retry_never_retries_a_schema_error() {
    let client = InfoClient::new(RetryTransport::new([RetryResponse::Body(
        br#"{"unexpected":true}"#.to_vec(),
    )]));

    let error = block_on(client.funding_history_range_with_retry(
        "BTC",
        TimestampMs::new(1_000),
        TimestampMs::new(2_000),
        immediate_retry_policy(3),
    ))
    .expect_err("schema failures must fail immediately");

    assert!(matches!(
        error,
        FundingHistoryPaginationError::Request(InfoClientError::Decode(_))
    ));
    assert_eq!(client.transport().request_count(), 1);
}

#[derive(Debug)]
struct StaticTransport(&'static [u8]);

impl InfoTransport for StaticTransport {
    fn network(&self) -> Network {
        Network::Mainnet
    }

    #[allow(clippy::unused_async_trait_impl)]
    async fn post(&self, _request_body: &[u8]) -> Result<Vec<u8>, BoxError> {
        Ok(self.0.to_vec())
    }
}

#[test]
fn malformed_fixture_is_reported_as_a_decode_error() {
    let client = InfoClient::new(StaticTransport(br#"{"not":"funding history"}"#));

    let error = block_on(client.execute(&FundingHistoryRequest::new("BTC", TimestampMs::new(0))))
        .expect_err("the response shape is invalid");

    assert!(matches!(error, InfoClientError::Decode(_)));
}

#[test]
fn networks_own_their_official_info_endpoints_and_stable_names() {
    assert_eq!(Network::Mainnet.to_string(), "mainnet");
    assert_eq!(Network::Testnet.to_string(), "testnet");
    assert_eq!(
        Network::Mainnet.info_endpoint(),
        "https://api.hyperliquid.xyz/info"
    );
    assert_eq!(
        Network::Testnet.info_endpoint(),
        "https://api.hyperliquid-testnet.xyz/info"
    );
    assert_eq!(
        Network::Mainnet.websocket_endpoint(),
        "wss://api.hyperliquid.xyz/ws"
    );
    assert_eq!(
        Network::Testnet.websocket_endpoint(),
        "wss://api.hyperliquid-testnet.xyz/ws"
    );
    assert_eq!("mainnet".parse(), Ok(Network::Mainnet));
    assert_eq!("testnet".parse(), Ok(Network::Testnet));
    assert!("production".parse::<Network>().is_err());
    assert_eq!(
        serde_json::to_string(&Network::Testnet).expect("network serializes"),
        r#""testnet""#
    );
}

#[test]
fn production_transport_requires_an_explicit_network() {
    let mainnet =
        ReqwestInfoTransport::for_network(Network::Mainnet).expect("default async client builds");
    let testnet =
        ReqwestInfoTransport::for_network(Network::Testnet).expect("default async client builds");

    assert_eq!(mainnet.network(), Network::Mainnet);
    assert_eq!(
        mainnet.endpoint().as_str(),
        Network::Mainnet.info_endpoint()
    );
    assert_eq!(testnet.network(), Network::Testnet);
    assert_eq!(
        testnet.endpoint().as_str(),
        Network::Testnet.info_endpoint()
    );
}

#[test]
fn development_endpoints_require_https_except_on_loopback() {
    let client = reqwest::Client::new();
    let localhost = ReqwestInfoTransport::for_development(
        Network::Testnet,
        client.clone(),
        Url::parse("http://localhost:8080/info").expect("test URL parses"),
    )
    .expect("localhost HTTP is allowed for tests");
    let loopback = ReqwestInfoTransport::for_development(
        Network::Testnet,
        client.clone(),
        Url::parse("http://127.0.0.1:8080/info").expect("test URL parses"),
    )
    .expect("loopback HTTP is allowed for tests");
    let secure_proxy = ReqwestInfoTransport::for_development(
        Network::Mainnet,
        client.clone(),
        Url::parse("https://proxy.example/info").expect("test URL parses"),
    )
    .expect("non-loopback HTTPS is allowed");
    let insecure_proxy = ReqwestInfoTransport::for_development(
        Network::Mainnet,
        client.clone(),
        Url::parse("http://proxy.example/info").expect("test URL parses"),
    );
    let non_http_loopback = ReqwestInfoTransport::for_development(
        Network::Testnet,
        client,
        Url::parse("ftp://127.0.0.1/info").expect("test URL parses"),
    );

    assert_eq!(localhost.network(), Network::Testnet);
    assert_eq!(loopback.network(), Network::Testnet);
    assert_eq!(secure_proxy.network(), Network::Mainnet);
    assert!(insecure_proxy.is_err());
    assert!(non_http_loopback.is_err());
}

#[test]
fn http_status_error_preserves_endpoint_and_retry_context_without_a_body() {
    let endpoint = Url::parse(Network::Testnet.info_endpoint()).expect("official URL parses");
    let client_error = InfoClientError::Transport(Box::new(HttpTransportError::Status {
        endpoint: endpoint.clone(),
        status: reqwest::StatusCode::TOO_MANY_REQUESTS,
        retry_after: Some("7".to_owned()),
    }));
    let error = client_error
        .http_transport_error()
        .expect("typed HTTP context remains available through the client error");

    assert_eq!(error.endpoint(), &endpoint);
    assert_eq!(error.status(), Some(reqwest::StatusCode::TOO_MANY_REQUESTS));
    assert_eq!(error.retry_after(), Some("7"));
    assert!(error.is_rate_limited());
    assert!(!error.is_timeout());
    assert_eq!(
        error.to_string(),
        format!("request to {endpoint} returned HTTP 429 Too Many Requests; retry-after=7")
    );
}

#[derive(Debug)]
struct PendingTransport {
    cancelled: Arc<AtomicBool>,
}

impl InfoTransport for PendingTransport {
    fn network(&self) -> Network {
        Network::Testnet
    }

    fn post(&self, _request_body: &[u8]) -> impl Future<Output = Result<Vec<u8>, BoxError>> + Send {
        PendingResponse {
            cancelled: Arc::clone(&self.cancelled),
        }
    }
}

struct PendingResponse {
    cancelled: Arc<AtomicBool>,
}

impl Future for PendingResponse {
    type Output = Result<Vec<u8>, BoxError>;

    fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}

impl Drop for PendingResponse {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

#[test]
fn dropping_execute_cancels_the_in_flight_transport_future() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let client = InfoClient::new(PendingTransport {
        cancelled: Arc::clone(&cancelled),
    });
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);

    {
        let mut request = std::pin::pin!(client.execute(&MetaAndAssetCtxsRequest));
        assert!(matches!(request.as_mut().poll(&mut context), Poll::Pending));
    }

    assert!(cancelled.load(Ordering::SeqCst));
}

#[test]
fn timestamp_serializes_as_a_plain_millisecond_integer() {
    assert_eq!(
        serde_json::to_string(&TimestampMs::new(1_683_849_600_048)).expect("timestamp serializes"),
        "1683849600048"
    );
}
