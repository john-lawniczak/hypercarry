use crate::SigningRequest;
use anyhow::{Context, Result, bail, ensure};
use hypercarry_execution::{ClientOrderId, ExecutionNetwork, Side};
use hypercarry_mainnet_config::{RuntimeConfig, validate_private_path};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Reviewed service configuration, kept outside the analytics dependency graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServicePolicy {
    pub schema_version: u32,
    /// Absolute executable receiving one prehash on stdin and returning a hex
    /// recoverable signature. Its key custody is external to this workspace.
    pub backend: PathBuf,
    pub backend_sha256: String,
    pub signer_sha256: String,
    pub watchdog_sha256: String,
    pub backend_timeout_ms: u64,
    pub signer_state: PathBuf,
    pub watchdog_state: PathBuf,
    pub watchdog_interval_ms: u64,
    /// Venue re-arm cadence, deliberately separate from the lease poll interval.
    /// Venue actions are rate limited per address, so re-arming on every poll
    /// would spend the same budget the emergency cancel itself needs.
    pub rearm_interval_ms: u64,
    pub cancel_horizon_ms: u64,
    /// External executor-host liveness lease. The watchdog must run on a
    /// separate failure domain; losing this lease starts emergency recovery.
    pub executor_lease: PathBuf,
    pub executor_lease_timeout_ms: u64,
    /// Explicitly reviewed price collar for reduce-only IOC emergency orders.
    #[serde(with = "rust_decimal::serde::str")]
    pub emergency_min_price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub emergency_max_price: Decimal,
}

impl ServicePolicy {
    /// # Errors
    /// Rejects invalid service bounds, paths or runtime bindings.
    pub fn validate(&self, runtime: &RuntimeConfig) -> Result<()> {
        ensure!(
            runtime.executor_lease.as_ref() == Some(&self.executor_lease),
            "executor lease is not bound to runtime"
        );
        ensure!(self.schema_version == 1, "unsupported service policy");
        ensure!(
            (100..=60_000).contains(&self.backend_timeout_ms),
            "invalid backend timeout"
        );
        ensure!(
            (100..=1_000).contains(&self.watchdog_interval_ms),
            "watchdog interval must be 100ms..1s"
        );
        ensure!(
            (30_000..=120_000).contains(&self.cancel_horizon_ms),
            "cancel horizon must be 30..120 seconds"
        );
        // Re-arming must be frequent enough that protection never lapses, and
        // rare enough that a long session cannot exhaust the address's venue
        // action budget before an emergency needs it. Capping the cadence at a
        // third of the horizon, together with the heartbeat bound below, also
        // guarantees that the protection still standing when a re-arm becomes
        // due outlasts the executor's trust in the heartbeat: the worst-case
        // remainder is 2/3 of the horizon against a timeout under 1/2 of it.
        ensure!(
            (1_000..=30_000).contains(&self.rearm_interval_ms)
                && self.rearm_interval_ms >= self.watchdog_interval_ms
                && self.rearm_interval_ms <= self.cancel_horizon_ms / 3,
            "re-arm cadence must be 1..30s, at least one poll interval, and at most a third of the cancel horizon"
        );
        ensure!(
            (1_000..=30_000).contains(&self.executor_lease_timeout_ms),
            "invalid executor lease timeout"
        );
        ensure!(
            runtime.heartbeat_timeout_ms < self.cancel_horizon_ms / 2,
            "heartbeat must expire before venue protection"
        );
        ensure!(
            self.emergency_min_price > Decimal::ZERO
                && self.emergency_max_price >= self.emergency_min_price,
            "invalid emergency collar"
        );
        // The watchdog submits a collar bound verbatim as the emergency limit
        // price, and `flatten_action` rejects an unquantized price. An unaligned
        // bound would therefore fail only at the moment flattening is required,
        // so it has to be refused when the policy is loaded instead.
        ensure!(
            self.emergency_min_price % runtime.metadata.price_tick == Decimal::ZERO
                && self.emergency_max_price % runtime.metadata.price_tick == Decimal::ZERO,
            "emergency collar must be quantized to the reviewed price tick"
        );
        for hash in [
            &self.backend_sha256,
            &self.signer_sha256,
            &self.watchdog_sha256,
        ] {
            ensure!(
                hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid artifact hash"
            );
        }
        let mut paths = vec![
            &runtime.signer_socket,
            &runtime.journal,
            &runtime.health_file,
            &runtime.heartbeat,
            &runtime.kill_switch,
        ];
        for path in [
            &self.signer_state,
            &self.watchdog_state,
            &self.executor_lease,
        ] {
            ensure!(
                path.is_absolute() && !paths.contains(&path),
                "service paths must be distinct absolute paths"
            );
            validate_private_path(path)?;
            paths.push(path);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Placement,
    Cancel,
    Schedule,
    Flatten,
}

/// # Errors
/// Rejects invalid order identity.
pub fn exact_order(runtime: &RuntimeConfig) -> Result<Value> {
    Ok(
        json!({"type":"order", "orders":[{"a":runtime.asset_id,"b":runtime.order.side == Side::Buy,
        "p":runtime.order.limit_price.normalize().to_string(),"s":runtime.order.quantity.normalize().to_string(),
        "r":false,"t":{"limit":{"tif":"Gtc"}},"c":ClientOrderId::derive(&runtime.order)?}],"grouping":"na"}),
    )
}

/// # Errors
/// Rejects expired requests, wrong identities and actions outside reviewed scope.
pub fn validate_request(
    request: &SigningRequest,
    runtime: &RuntimeConfig,
    policy: &ServicePolicy,
    now: u64,
) -> Result<Purpose> {
    ensure!(
        request.schema_version == 1 && request.network == ExecutionNetwork::Mainnet,
        "mainnet schema required"
    );
    ensure!(
        request.key_id == runtime.signer_alias && request.signer_address == runtime.signer_address,
        "signer identity mismatch"
    );
    ensure!(
        request.nonce.abs_diff(now) <= 5_000
            && request.expires_after > now
            && request.expires_after > request.nonce
            && request.expires_after - request.nonce <= runtime.action_ttl_ms,
        "expired or invalid signing window"
    );
    if request.action == exact_order(runtime)? {
        return Ok(Purpose::Placement);
    }
    let cancel = json!({"type":"cancelByCloid","cancels":[{"asset":runtime.asset_id,"cloid":ClientOrderId::derive(&runtime.order)?}]});
    if request.action == cancel {
        return Ok(Purpose::Cancel);
    }
    if request.action.get("type").and_then(Value::as_str) == Some("scheduleCancel") {
        let at = request
            .action
            .get("time")
            .and_then(Value::as_u64)
            .context("disarming is not permitted")?;
        ensure!(
            request.action == json!({"type":"scheduleCancel","time":at}),
            "unexpected schedule fields"
        );
        ensure!(
            at >= now.saturating_add(5_000) && at <= now.saturating_add(policy.cancel_horizon_ms),
            "scheduled deadline outside reviewed horizon"
        );
        return Ok(Purpose::Schedule);
    }
    // Emergency flattening is bounded by the canary size, fixed market, opposite
    // side and venue-enforced reduceOnly. It cannot open or reverse a position.
    let orders = request
        .action
        .get("orders")
        .and_then(Value::as_array)
        .context("unsupported action")?;
    ensure!(orders.len() == 1, "exactly one emergency order required");
    let order = &orders[0];
    let price: Decimal = order
        .get("p")
        .and_then(Value::as_str)
        .context("price must be decimal string")?
        .parse()?;
    let size: Decimal = order
        .get("s")
        .and_then(Value::as_str)
        .context("size must be decimal string")?
        .parse()?;
    let expected = flatten_action(runtime, price, size)?;
    if request.action != expected {
        bail!("action exceeds exact emergency scope");
    }
    ensure!(
        price >= policy.emergency_min_price && price <= policy.emergency_max_price,
        "emergency price outside collar"
    );
    Ok(Purpose::Flatten)
}

/// # Errors
/// Rejects non-positive, oversized or unquantized emergency orders.
pub fn flatten_action(runtime: &RuntimeConfig, price: Decimal, size: Decimal) -> Result<Value> {
    ensure!(
        size > Decimal::ZERO && size <= runtime.order.quantity,
        "emergency size exceeds reviewed canary"
    );
    ensure!(
        size % runtime.metadata.size_step == Decimal::ZERO
            && price % runtime.metadata.price_tick == Decimal::ZERO,
        "unquantized emergency order"
    );
    Ok(
        json!({"type":"order","orders":[{"a":runtime.asset_id,"b":runtime.order.side != Side::Buy,
        "p":price.normalize().to_string(),"s":size.normalize().to_string(),"r":true,"t":{"limit":{"tif":"Ioc"}}}],"grouping":"na"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (RuntimeConfig, ServicePolicy, SigningRequest) {
        let r: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../docs/mainnet-runtime-config.example.json"
        ))
        .unwrap();
        let p = ServicePolicy {
            schema_version: 1,
            backend: "/private/backend".into(),
            backend_sha256: "0".repeat(64),
            signer_sha256: "0".repeat(64),
            watchdog_sha256: "0".repeat(64),
            backend_timeout_ms: 1000,
            signer_state: "/private/signer.json".into(),
            watchdog_state: "/private/watchdog.json".into(),
            watchdog_interval_ms: 1000,
            rearm_interval_ms: 10000,
            cancel_horizon_ms: 30000,
            executor_lease: "/private/lease.json".into(),
            executor_lease_timeout_ms: 5000,
            emergency_min_price: Decimal::from(90000),
            emergency_max_price: Decimal::from(110_000),
        };
        let request = SigningRequest {
            schema_version: 1,
            network: ExecutionNetwork::Mainnet,
            key_id: r.signer_alias.clone(),
            signer_address: r.signer_address.clone(),
            nonce: 10000,
            expires_after: 20000,
            action: exact_order(&r).unwrap(),
        };
        (r, p, request)
    }
    #[test]
    fn signer_accepts_only_exact_canary_and_rejects_extra_fields() {
        let (r, p, mut request) = fixture();
        assert_eq!(
            validate_request(&request, &r, &p, 10000).unwrap(),
            Purpose::Placement
        );
        request.action["orders"][0]["s"] = json!("10");
        assert!(validate_request(&request, &r, &p, 10000).is_err());
        request.action = exact_order(&r).unwrap();
        request.action["builder"] = json!({});
        assert!(validate_request(&request, &r, &p, 10000).is_err());
        request.action = exact_order(&r).unwrap();
        request.network = ExecutionNetwork::Testnet;
        assert!(validate_request(&request, &r, &p, 10000).is_err());
    }
    #[test]
    fn emergency_is_reduce_only_opposite_side_ioc_inside_reviewed_collar() {
        let (r, p, mut request) = fixture();
        request.action = flatten_action(&r, Decimal::from(90000), r.order.quantity).unwrap();
        assert_eq!(
            validate_request(&request, &r, &p, 10000).unwrap(),
            Purpose::Flatten
        );
        for (field, value) in [
            ("r", json!(false)),
            ("b", json!(true)),
            ("a", json!(1)),
            ("p", json!("89999")),
            ("s", json!("1")),
            ("t", json!({"limit":{"tif":"Gtc"}})),
        ] {
            let action = request.action.clone();
            request.action["orders"][0][field] = value;
            assert!(
                validate_request(&request, &r, &p, 10000).is_err(),
                "{field}"
            );
            request.action = action;
        }
    }
    #[test]
    fn scheduled_cancel_cannot_be_disarmed_or_extended_past_reviewed_horizon() {
        let (r, p, mut request) = fixture();
        request.action = json!({"type":"scheduleCancel","time":40000});
        assert_eq!(
            validate_request(&request, &r, &p, 10000).unwrap(),
            Purpose::Schedule
        );
        for action in [
            json!({"type":"scheduleCancel"}),
            json!({"type":"scheduleCancel","time":14999}),
            json!({"type":"scheduleCancel","time":40001}),
        ] {
            request.action = action;
            assert!(validate_request(&request, &r, &p, 10000).is_err());
        }
    }
    /// The watchdog submits a collar bound verbatim as the emergency limit price,
    /// so an unquantized bound would fail only while flattening a live position.
    #[test]
    fn an_unquantized_emergency_collar_is_refused_before_it_is_needed() {
        let (mut r, mut p, _) = fixture();
        r.executor_lease = Some(p.executor_lease.clone());
        p.emergency_min_price = Decimal::from(90_000);
        p.emergency_max_price = Decimal::from(110_000);
        assert!(flatten_action(&r, p.emergency_min_price, r.order.quantity).is_ok());
        assert!(flatten_action(&r, p.emergency_max_price, r.order.quantity).is_ok());

        // A tick of 1 cannot represent a fractional bound, and `flatten_action`
        // would reject it at exactly the wrong moment.
        p.emergency_min_price = "90000.5".parse().unwrap();
        assert!(flatten_action(&r, p.emergency_min_price, r.order.quantity).is_err());
        assert!(
            p.validate(&r)
                .unwrap_err()
                .to_string()
                .contains("price tick")
        );
    }

    /// The reviewed cadence has to keep venue protection continuous: re-arming
    /// must fit inside the horizon alongside the executor's heartbeat timeout.
    #[test]
    fn rearm_cadence_bounds_reject_a_lapse_or_a_per_poll_venue_write() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let (mut r, mut base, _) = fixture();
        base.signer_state = temp.path().join("signer.json");
        base.watchdog_state = temp.path().join("watchdog.json");
        base.executor_lease = temp.path().join("lease.json");
        r.executor_lease = Some(base.executor_lease.clone());
        assert!(base.validate(&r).is_ok(), "{:?}", base.validate(&r).err());

        for (interval, reason) in [
            (999_u64, "below one second"),
            (0, "zero"),
            (31_000, "above thirty seconds"),
            // More than a third of a 30s horizon leaves no margin for a retry.
            (11_000, "beyond a third of the horizon"),
            // Below the poll interval means a venue write on every single poll.
            (500, "under the poll interval"),
        ] {
            let mut p = base.clone();
            p.rearm_interval_ms = interval;
            assert!(p.validate(&r).is_err(), "accepted {reason}: {interval}ms");
        }

        // The cadence cap and the heartbeat bound together guarantee that the
        // protection remaining when a re-arm becomes due still outlasts the
        // executor's trust window, for every cadence this validator accepts.
        for horizon in [30_000_u64, 60_000, 120_000] {
            let mut p = base.clone();
            p.cancel_horizon_ms = horizon;
            // The slowest cadence this validator will accept for that horizon.
            p.rearm_interval_ms = (horizon / 3).min(30_000);
            let mut r = r.clone();
            r.heartbeat_timeout_ms = horizon / 2 - 1;
            assert!(p.validate(&r).is_ok(), "{horizon}ms horizon rejected");
            assert!(
                horizon - p.rearm_interval_ms > r.heartbeat_timeout_ms,
                "protection can lapse inside the heartbeat window at {horizon}ms"
            );
        }
    }

    #[test]
    fn expired_and_wrong_signer_requests_never_reach_backend() {
        let (r, p, mut request) = fixture();
        assert!(validate_request(&request, &r, &p, 20000).is_err());
        request.signer_address = r.account_address.clone();
        assert!(validate_request(&request, &r, &p, 10000).is_err());
    }
}
