//! Independent watchdog: venue-side cancellation plus bounded reduce-only
//! recovery. An acknowledgement is never treated as proof that a position is flat.
use crate::{
    SigningRequest, now_ms,
    policy::{ServicePolicy, flatten_action, validate_request},
    require_artifact, write_private,
};
use alloy::primitives::Signature;
use anyhow::{Context, Result, ensure};
use chrono::DateTime;
use fs2::FileExt;
use hypercarry_execution::{ClientOrderId, ExecutionNetwork, Side};
use hypercarry_mainnet_config::{RuntimeConfig, read_json, validate_private_path};
use hypersdk::hypercore::{Action, Chain};
use reqwest::blocking::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::{fs::OpenOptionsExt, net::UnixStream},
    str::FromStr,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub schema_version: u32,
    pub network: ExecutionNetwork,
    pub integration_config_digest: String,
    pub observed_at_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WatchdogState {
    binding: String,
    last_nonce: u64,
    armed_until_ms: Option<u64>,
    emergency: bool,
    flat: bool,
}

pub fn lease_is_fresh(lease: &Lease, binding: &str, now: u64, max_age: u64) -> bool {
    lease.schema_version == 1
        && lease.network == ExecutionNetwork::Mainnet
        && lease.integration_config_digest == binding
        && lease.observed_at_ms <= now
        && now - lease.observed_at_ms <= max_age
}

/// Whether the executor currently holds a trustworthy liveness lease.
///
/// A lease that cannot be read, is not a regular file, fails strict parsing, or
/// carries another runtime's binding is not a lease. An unavailable clock reads
/// as the far future, which expires every lease rather than extending one.
fn lease_held(policy: &ServicePolicy, binding: &str) -> bool {
    read_json::<Lease>(&policy.executor_lease).is_ok_and(|lease| {
        lease_is_fresh(
            &lease,
            binding,
            now_ms().unwrap_or(u64::MAX),
            policy.executor_lease_timeout_ms,
        )
    })
}

/// Whether the venue deadline has decayed by a full re-arm interval.
///
/// The lease is polled far more often than the venue is written to, because a
/// venue action is rate limited per address while reading a local file is not.
/// Re-arming on every poll would spend, over a long session, the same action
/// budget the emergency cancel depends on.
pub fn rearm_due(armed_until_ms: Option<u64>, now: u64, policy: &ServicePolicy) -> bool {
    armed_until_ms.is_none_or(|until| {
        until.saturating_sub(now)
            <= policy
                .cancel_horizon_ms
                .saturating_sub(policy.rearm_interval_ms)
    })
}

/// Whether a fresh heartbeat may be written for an already-armed deadline.
///
/// The executor trusts a heartbeat for its whole timeout, so it may act at any
/// point up to `heartbeat_timeout_ms` after this write. Venue protection has to
/// still be armed then; otherwise the heartbeat promises cover that has lapsed.
pub fn heartbeat_permitted(
    armed_until_ms: Option<u64>,
    now: u64,
    heartbeat_timeout_ms: u64,
) -> bool {
    armed_until_ms.is_some_and(|until| until >= now.saturating_add(heartbeat_timeout_ms))
}

fn post(client: &Client, endpoint: &str, body: &Value) -> Result<Value> {
    let response = client
        .post(endpoint)
        .json(body)
        .send()?
        .error_for_status()?;
    let mut bytes = Vec::new();
    response.take(1_048_577).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1_048_576, "venue response exceeds bound");
    Ok(serde_json::from_slice(&bytes)?)
}
fn info(client: &Client, body: &Value) -> Result<Value> {
    post(client, "https://api.hyperliquid.xyz/info", body)
}

/// Verifies the socket's response cryptographically before it reaches exchange.
fn signed_request(request: &SigningRequest, runtime: &RuntimeConfig) -> Result<Value> {
    hypercarry_executor::signer::validate_socket(&runtime.signer_socket)?;
    let mut stream = UnixStream::connect(&runtime.signer_socket)?;
    stream.set_read_timeout(Some(Duration::from_millis(runtime.signer_timeout_ms)))?;
    stream.set_write_timeout(Some(Duration::from_millis(runtime.signer_timeout_ms)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut bytes = Vec::new();
    stream.take(65_537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65_536, "oversized signer response");
    let response: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        response["schema_version"] == 1
            && response["network"] == "mainnet"
            && response["key_id"] == runtime.signer_alias
            && response["signer_address"] == runtime.signer_address,
        "signer identity mismatch"
    );
    let signed = response
        .get("signed_request")
        .context("missing signed request")?;
    ensure!(
        signed["nonce"] == request.nonce
            && signed["expiresAfter"] == request.expires_after
            && signed["vaultAddress"].is_null(),
        "signed domain fields mismatch"
    );
    let action: Action = serde_json::from_value(request.action.clone())?;
    ensure!(
        signed["action"] == serde_json::to_value(&action)?,
        "signer altered action"
    );
    ensure!(
        signed.as_object().is_some_and(|o| o.len() == 5),
        "unexpected signed fields"
    );
    let hash = action.prehash(
        request.nonce,
        None,
        Some(
            DateTime::from_timestamp_millis(i64::try_from(request.expires_after)?)
                .context("invalid expiry")?,
        ),
        Chain::Mainnet,
    )?;
    let sig = &signed["signature"];
    let r = sig["r"].as_str().context("missing signature r")?;
    let s = sig["s"].as_str().context("missing signature s")?;
    ensure!(
        r.len() == 66 && s.len() == 66 && r.starts_with("0x") && s.starts_with("0x"),
        "invalid signature scalars"
    );
    let v = sig["v"].as_u64().context("missing recovery ID")?;
    ensure!(v == 27 || v == 28, "invalid recovery ID");
    let signature = Signature::from_str(&format!("{r}{}{:02x}", &s[2..], v))?;
    ensure!(
        signature
            .recover_address_from_prehash(&hash)?
            .to_string()
            .to_ascii_lowercase()
            == runtime.signer_address,
        "signature recovered wrong signer"
    );
    ensure!(now_ms()? < request.expires_after, "signature expired");
    Ok(signed.clone())
}

fn submit(
    client: &Client,
    runtime: &RuntimeConfig,
    policy: &ServicePolicy,
    state: &mut WatchdogState,
    action: Value,
) -> Result<Value> {
    let now = now_ms()?;
    let nonce = now.max(state.last_nonce.checked_add(1).context("nonce overflow")?);
    let request = SigningRequest {
        schema_version: 1,
        network: ExecutionNetwork::Mainnet,
        key_id: runtime.signer_alias.clone(),
        signer_address: runtime.signer_address.clone(),
        nonce,
        expires_after: nonce
            .checked_add(runtime.action_ttl_ms)
            .context("expiry overflow")?,
        action,
    };
    validate_request(&request, runtime, policy, now)?;
    state.last_nonce = nonce;
    write_private(&policy.watchdog_state, state)?;
    crate::check_agent(client, runtime)?;
    let signed = signed_request(&request, runtime)?;
    validate_request(&request, runtime, policy, now_ms()?)?;
    crate::check_agent(client, runtime)?;
    validate_request(&request, runtime, policy, now_ms()?)?;
    let response = post(client, "https://api.hyperliquid.xyz/exchange", &signed)?;
    ensure!(
        response["status"] == "ok",
        "emergency action rejected; outcome must be reconciled"
    );
    Ok(response)
}

/// Recovery checks all perp DEX positions. Only the reviewed default-DEX market
/// can be flattened. Any other position remains an explicit operator incident.
fn account_position(client: &Client, runtime: &RuntimeConfig) -> Result<Decimal> {
    let dexs = info(client, &json!({"type":"perpDexs"}))?;
    let dexs = dexs.as_array().context("invalid DEX list")?;
    ensure!(
        dexs.first().is_some_and(Value::is_null),
        "missing default DEX"
    );
    let mut quantity = Decimal::ZERO;
    for dex in dexs {
        let mut query = json!({"type":"clearinghouseState","user":runtime.account_address});
        if !dex.is_null() {
            query["dex"] = dex.get("name").context("missing DEX name")?.clone();
        }
        let state = info(client, &query)?;
        let mut order_query = query.clone();
        order_query["type"] = json!("openOrders");
        let orders = info(client, &order_query)?;
        ensure!(
            orders.as_array().is_some_and(Vec::is_empty),
            "open orders remain on a perp DEX; manual recovery required"
        );
        for entry in state["assetPositions"]
            .as_array()
            .context("missing positions")?
        {
            let position = &entry["position"];
            let size: Decimal = position["szi"]
                .as_str()
                .context("missing position size")?
                .parse()?;
            if size == Decimal::ZERO {
                continue;
            }
            ensure!(
                dex.is_null() && position["coin"] == runtime.order.market,
                "out-of-scope position: manual recovery required"
            );
            ensure!(quantity == Decimal::ZERO, "duplicate position");
            quantity = size;
        }
    }
    Ok(quantity)
}

fn emergency(
    client: &Client,
    runtime: &RuntimeConfig,
    policy: &ServicePolicy,
    state: &mut WatchdogState,
) -> Result<()> {
    state.emergency = true;
    state.flat = false;
    write_private(&policy.watchdog_state, state)?;
    // Prevent new risk before attempting any network operation.
    write_private(
        &runtime.kill_switch,
        &json!({"reason":"watchdog_emergency","observed_at_ms":now_ms()?}),
    )?;
    let _ = fs::remove_file(&runtime.heartbeat);
    let cancel = json!({"type":"cancelByCloid","cancels":[{"asset":runtime.asset_id,"cloid":ClientOrderId::derive(&runtime.order)?}]});
    submit(client, runtime, policy, state, cancel)?;
    // Never flatten while an entry order might still fill behind us.
    let orders = info(
        client,
        &json!({"type":"openOrders","user":runtime.account_address}),
    )?;
    ensure!(
        orders.as_array().is_some_and(Vec::is_empty),
        "orders remain open; manual reconciliation required"
    );
    let position = account_position(client, runtime)?;
    if position != Decimal::ZERO {
        ensure!(
            (position > Decimal::ZERO) == (runtime.order.side == Side::Buy),
            "position direction outside reviewed canary"
        );
        let price = if position > Decimal::ZERO {
            policy.emergency_min_price
        } else {
            policy.emergency_max_price
        };
        let size = if position < Decimal::ZERO {
            Decimal::ZERO
                .checked_sub(position)
                .context("position size overflow")?
        } else {
            position
        };
        let action = flatten_action(runtime, price, size)?;
        // One bounded IOC attempt. Ambiguous or partial outcomes are retained;
        // no automatic resubmission, reversal or expanding price collar.
        submit(client, runtime, policy, state, action)?;
    }
    state.flat = account_position(client, runtime)? == Decimal::ZERO;
    write_private(&policy.watchdog_state, state)?;
    ensure!(
        state.flat,
        "position remains after bounded flatten; manual recovery required"
    );
    Ok(())
}

/// # Errors
/// Rejects unsafe state, unreviewed artifacts or unresolved emergency recovery.
pub fn run(
    runtime: &RuntimeConfig,
    policy: &ServicePolicy,
    stop: &AtomicBool,
    recover: bool,
) -> Result<()> {
    require_artifact(&std::env::current_exe()?, &policy.watchdog_sha256)?;
    let binding = runtime.digest()?;
    let lock_path = policy.watchdog_state.with_extension("lock");
    validate_private_path(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    lock.try_lock_exclusive()
        .context("watchdog already running")?;
    let mut state: WatchdogState = if policy.watchdog_state.exists() {
        read_json(&policy.watchdog_state)?
    } else {
        WatchdogState {
            binding: binding.clone(),
            ..WatchdogState::default()
        }
    };
    ensure!(
        state.binding == binding,
        "watchdog state belongs to another runtime"
    );
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()?;
    if recover {
        return emergency(&client, runtime, policy, &mut state);
    }
    ensure!(
        !state.emergency,
        "previous emergency requires explicit --recover; will not resume arming"
    );
    // Never reuse a heartbeat from an earlier process.
    let _ = fs::remove_file(&runtime.heartbeat);
    let mut started = state.armed_until_ms.is_some();
    while !stop.load(Ordering::Relaxed) {
        if !lease_held(policy, &binding) {
            if started {
                return emergency(&client, runtime, policy, &mut state);
            }
            thread::sleep(Duration::from_millis(policy.watchdog_interval_ms));
            continue;
        }
        if runtime.kill_switch.exists() {
            return emergency(&client, runtime, policy, &mut state);
        }
        if rearm_due(state.armed_until_ms, now_ms()?, policy) {
            let until = now_ms()?
                .checked_add(policy.cancel_horizon_ms)
                .context("deadline overflow")?;
            if submit(
                &client,
                runtime,
                policy,
                &mut state,
                json!({"type":"scheduleCancel","time":until}),
            )
            .is_err()
            {
                let _ = fs::remove_file(&runtime.heartbeat);
                return emergency(&client, runtime, policy, &mut state);
            }
            state.armed_until_ms = Some(until);
            write_private(&policy.watchdog_state, &state)?;
            // Lease/STOP may change during signer/venue I/O. Acknowledgement
            // alone must not renew protection for an executor that has died.
            if !lease_held(policy, &binding)
                || runtime.kill_switch.exists()
                || stop.load(Ordering::Relaxed)
            {
                return emergency(&client, runtime, policy, &mut state);
            }
        }
        // Only venue protection that is acknowledged, and that outlasts the
        // executor's trust in this file, permits a fresh heartbeat.
        if !heartbeat_permitted(
            state.armed_until_ms,
            now_ms()?,
            runtime.heartbeat_timeout_ms,
        ) {
            let _ = fs::remove_file(&runtime.heartbeat);
            return emergency(&client, runtime, policy, &mut state);
        }
        let until = state.armed_until_ms.context("armed deadline is unknown")?;
        write_private(
            &runtime.heartbeat,
            &json!({"schema_version":1,"armed_until_ms":until,"integration_config_digest":binding}),
        )?;
        started = true;
        thread::sleep(Duration::from_millis(policy.watchdog_interval_ms));
    }
    if started {
        emergency(&client, runtime, policy, &mut state)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> ServicePolicy {
        serde_json::from_str(include_str!(
            "../../../docs/mainnet-services-policy.example.json"
        ))
        .unwrap()
    }

    /// Protection must be pushed back well before it lapses, but writing to the
    /// venue on every lease poll would spend a rate-limited action budget the
    /// emergency cancel later needs.
    #[test]
    fn venue_rearming_is_paced_independently_of_lease_polling() {
        let policy = policy();
        assert!(rearm_due(None, 1_000, &policy));
        let armed = 1_000 + policy.cancel_horizon_ms;
        // Immediately after arming, and for most of the horizon, nothing is due.
        assert!(!rearm_due(Some(armed), 1_000, &policy));
        assert!(!rearm_due(
            Some(armed),
            1_000 + policy.rearm_interval_ms - 1,
            &policy
        ));
        // One full re-arm interval of the horizon has now elapsed.
        assert!(rearm_due(
            Some(armed),
            1_000 + policy.rearm_interval_ms,
            &policy
        ));
        // A deadline already in the past can never defer re-arming.
        assert!(rearm_due(Some(armed), armed + 1, &policy));
    }

    /// The executor acts on a heartbeat for its whole timeout, so a heartbeat
    /// written against protection that expires inside that window would promise
    /// cover the venue no longer holds.
    #[test]
    fn a_heartbeat_is_refused_once_protection_cannot_outlast_its_timeout() {
        let timeout = 5_000;
        assert!(!heartbeat_permitted(None, 1_000, timeout));
        assert!(heartbeat_permitted(Some(6_000), 1_000, timeout));
        assert!(!heartbeat_permitted(Some(5_999), 1_000, timeout));
        assert!(!heartbeat_permitted(Some(1_000), 1_000, timeout));
        // A clock far beyond the deadline must not wrap into permission.
        assert!(!heartbeat_permitted(Some(6_000), u64::MAX, timeout));
    }

    /// Every reviewed cadence must leave protection armed across the gap between
    /// re-arms, including the executor's trust window for the heartbeat.
    #[test]
    fn reviewed_cadence_keeps_protection_armed_between_rearms() {
        let runtime: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../docs/mainnet-runtime-config.example.json"
        ))
        .unwrap();
        let policy = policy();
        let now = 1_000;
        let armed = now + policy.cancel_horizon_ms;
        // At the latest moment a re-arm becomes due, a heartbeat is still valid:
        // protection never lapses between two consecutive venue writes.
        let latest = now + policy.rearm_interval_ms;
        assert!(rearm_due(Some(armed), latest, &policy));
        assert!(heartbeat_permitted(
            Some(armed),
            latest,
            runtime.heartbeat_timeout_ms
        ));
    }

    #[test]
    fn wrong_future_and_stale_leases_never_refresh_protection() {
        let mut lease = Lease {
            schema_version: 1,
            network: ExecutionNetwork::Mainnet,
            integration_config_digest: "reviewed".into(),
            observed_at_ms: 1000,
        };
        assert!(lease_is_fresh(&lease, "reviewed", 1500, 500));
        assert!(!lease_is_fresh(&lease, "reviewed", 1501, 500));
        assert!(!lease_is_fresh(&lease, "other", 1500, 500));
        lease.observed_at_ms = 1501;
        assert!(!lease_is_fresh(&lease, "reviewed", 1500, 500));
    }
}
