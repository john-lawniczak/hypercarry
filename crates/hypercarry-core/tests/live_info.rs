//! Opt-in smoke tests against Hyperliquid's public REST API.
//!
//! These tests are ignored by default so normal CI and local testing remain
//! deterministic and offline. Run explicitly with:
//!
//! `cargo test -p hypercarry-core --test live_info -- --ignored`

use hypercarry_core::{
    info::{
        FundingHistoryRequest, InfoClient, MetaAndAssetCtxsRequest, Network,
        PredictedFundingsRequest, ReqwestInfoTransport,
    },
    types::TimestampMs,
};
use std::{future::Future, time::SystemTime};

const FUNDING_WINDOW_MS: i64 = 4 * 60 * 60 * 1_000;

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("live-test runtime builds")
        .block_on(future)
}

async fn smoke_test_network(network: Network) -> Result<(), String> {
    let transport = ReqwestInfoTransport::for_network(network)
        .map_err(|error| format!("{network} client setup failed: {error}"))?;
    let endpoint = transport.endpoint().clone();
    let client = InfoClient::new(transport);

    let contexts = client
        .execute(&MetaAndAssetCtxsRequest)
        .await
        .map_err(|error| live_error(network, &endpoint, "metaAndAssetCtxs", &error))?;
    let snapshots = contexts.into_snapshots().map_err(|error| {
        format!("{network} {endpoint} metaAndAssetCtxs arrays do not align: {error:?}")
    })?;
    if snapshots.is_empty() {
        return Err(format!(
            "{network} {endpoint} metaAndAssetCtxs returned an empty universe"
        ));
    }
    if snapshots
        .iter()
        .any(|snapshot| snapshot.meta.name.is_empty())
    {
        return Err(format!(
            "{network} {endpoint} metaAndAssetCtxs returned an empty coin name"
        ));
    }
    if !snapshots
        .iter()
        .any(|snapshot| !snapshot.meta.is_delisted && snapshot.meta.name == "BTC")
    {
        return Err(format!(
            "{network} {endpoint} metaAndAssetCtxs did not contain active BTC"
        ));
    }

    let predictions = client
        .execute(&PredictedFundingsRequest)
        .await
        .map_err(|error| live_error(network, &endpoint, "predictedFundings", &error))?;
    if predictions.is_empty() {
        return Err(format!(
            "{network} {endpoint} predictedFundings returned no coins"
        ));
    }
    for prediction in &predictions {
        if prediction.0.is_empty() || prediction.1.iter().any(|venue| venue.0.is_empty()) {
            return Err(format!(
                "{network} {endpoint} predictedFundings returned an empty coin or venue name"
            ));
        }
        if prediction.1.iter().any(|venue| {
            venue
                .1
                .as_ref()
                .and_then(|value| value.funding_interval_hours)
                == Some(0)
        }) {
            return Err(format!(
                "{network} {endpoint} predictedFundings returned a zero-hour interval"
            ));
        }
    }

    let end_time = current_timestamp_ms()?;
    let start_time = TimestampMs::new(end_time.as_i64() - FUNDING_WINDOW_MS);
    let history = client
        .execute(&FundingHistoryRequest::new("BTC", start_time).with_end_time(end_time))
        .await
        .map_err(|error| live_error(network, &endpoint, "fundingHistory", &error))?;
    if history.is_empty() {
        return Err(format!(
            "{network} {endpoint} fundingHistory returned no BTC observations in four hours"
        ));
    }
    if history
        .iter()
        .any(|entry| entry.coin != "BTC" || entry.time < start_time || entry.time > end_time)
    {
        return Err(format!(
            "{network} {endpoint} fundingHistory violated coin or request-window invariants"
        ));
    }

    Ok(())
}

fn current_timestamp_ms() -> Result<TimestampMs, String> {
    let milliseconds = SystemTime::UNIX_EPOCH
        .elapsed()
        .map_err(|error| format!("system clock is before the Unix epoch: {error}"))?
        .as_millis();
    let milliseconds = i64::try_from(milliseconds)
        .map_err(|_| "current unix timestamp does not fit in i64 milliseconds".to_owned())?;
    Ok(TimestampMs::new(milliseconds))
}

fn live_error(
    network: Network,
    endpoint: &reqwest::Url,
    request_type: &str,
    error: &dyn std::error::Error,
) -> String {
    format!("{network} {endpoint} {request_type} smoke request failed: {error}")
}

#[test]
#[ignore = "opt-in live API test; requires internet and mutable exchange state"]
fn mainnet_info_smoke() -> Result<(), String> {
    block_on(smoke_test_network(Network::Mainnet))
}

#[test]
#[ignore = "opt-in live API test; requires internet and mutable exchange state"]
fn testnet_info_smoke() -> Result<(), String> {
    block_on(smoke_test_network(Network::Testnet))
}
