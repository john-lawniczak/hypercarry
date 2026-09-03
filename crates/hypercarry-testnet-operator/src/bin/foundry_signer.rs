//! Owner-only Foundry-keystore signer for bounded Hyperliquid testnet evidence.
//!
//! This process never reads a private key or password. It delegates one order
//! and its matching cancel prehash to `cast wallet sign`, which unlocks the
//! encrypted keystore interactively in this process's terminal.

#![warn(missing_docs)]

use alloy::primitives::{Signature as AlloySignature, U256};
use anyhow::{Context, Result, bail, ensure};
use chrono::DateTime;
use clap::{Parser, ValueEnum};
use hypercarry_execution::ExecutionNetwork;
use hypersdk::hypercore::{Action, Chain};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    flag,
};
use std::{
    collections::BTreeSet,
    fs,
    io::{ErrorKind, Read, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SIGNER_PROTOCOL_VERSION: u32 = 1;
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(60);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_CLOCK_SKEW_MS: u64 = 60_000;

#[derive(Debug, Parser)]
#[command(
    name = "hypercarry-foundry-testnet-signer",
    about = "Owner-only, testnet-only Foundry keystore signer"
)]
struct Cli {
    /// Owner-only Unix-domain socket exposed to the testnet operator.
    #[arg(long)]
    socket: PathBuf,

    /// Absolute encrypted Foundry keystore file; raw keys are not accepted.
    #[arg(long)]
    keystore: PathBuf,

    /// Absolute path to the reviewed cast executable.
    #[arg(long)]
    cast_bin: PathBuf,

    /// Non-secret alias that must match the operator configuration.
    #[arg(long)]
    key_id: String,

    /// Lowercase agent address recovered from every produced signature.
    #[arg(long)]
    signer_address: String,

    /// Only asset ID that this one-shot signer will authorize.
    #[arg(long)]
    allowed_asset: u32,

    /// Only order side that this one-shot signer will authorize.
    #[arg(long, value_enum)]
    allowed_side: AllowedSide,

    /// Maximum price multiplied by quantity accepted by the signer.
    #[arg(long)]
    max_notional: Decimal,

    /// Maximum request time-to-live accepted by the signer.
    #[arg(long, default_value_t = 60_000)]
    max_ttl_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AllowedSide {
    Buy,
    Sell,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignerRequest {
    schema_version: u32,
    network: ExecutionNetwork,
    key_id: String,
    signer_address: String,
    nonce: u64,
    expires_after: u64,
    action: Value,
}

#[derive(Debug, Serialize)]
struct SignerResponse<'a> {
    schema_version: u32,
    network: ExecutionNetwork,
    key_id: &'a str,
    signer_address: &'a str,
    signed_request: Value,
}

#[derive(Debug)]
struct Policy {
    key_id: String,
    signer_address: String,
    allowed_asset: u32,
    allowed_side: AllowedSide,
    max_notional: Decimal,
    max_ttl_ms: u64,
    cast_bin: PathBuf,
    keystore: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    Order,
    Cancel { cloid: String },
    Complete,
}

#[derive(Debug)]
struct ValidatedRequest {
    action: Action,
    next_phase: Phase,
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let policy = validate_cli(&cli)?;
    let shutdown = install_shutdown_handlers()?;
    let listener = bind_private_socket(&cli.socket)?;
    let _socket_guard = SocketGuard(cli.socket.clone());

    eprintln!(
        "testnet signer ready: key_id={} address={} socket={}",
        policy.key_id,
        policy.signer_address,
        cli.socket.display()
    );
    eprintln!("cast will request the encrypted-keystore password for each signature");

    let mut phase = Phase::Order;
    while phase != Phase::Complete {
        let Some(mut stream) = accept_until_shutdown(&listener, &shutdown)? else {
            eprintln!("signer interrupted; signer socket closed without advancing its phase");
            return Ok(());
        };
        let request = read_request(&mut stream)?;
        let validated = validate_request(&request, &policy, &phase)?;
        let signed_request = sign_request(&request, &validated.action, &policy, &shutdown)?;
        write_response(&mut stream, &request, &signed_request)?;
        phase = validated.next_phase;
        eprintln!(
            "signed bounded testnet {} request",
            if phase == Phase::Complete {
                "cancel"
            } else {
                "order"
            }
        );
    }

    eprintln!("one-shot signing session complete; signer socket closed");
    Ok(())
}

fn install_shutdown_handlers() -> Result<Arc<AtomicBool>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    for signal in [SIGINT, SIGTERM] {
        flag::register(signal, Arc::clone(&shutdown))
            .with_context(|| format!("could not register shutdown signal {signal}"))?;
    }
    Ok(shutdown)
}

fn accept_until_shutdown(
    listener: &UnixListener,
    shutdown: &AtomicBool,
) -> Result<Option<UnixStream>> {
    listener
        .set_nonblocking(true)
        .context("could not make signer listener interruptible")?;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .context("could not configure signer connection")?;
                return Ok(Some(stream));
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("could not accept signer connection"),
        }
    }
}

fn validate_cli(cli: &Cli) -> Result<Policy> {
    validate_alias(&cli.key_id)?;
    validate_address(&cli.signer_address)?;
    ensure!(
        cli.max_notional > Decimal::ZERO,
        "max notional must be positive"
    );
    ensure!(
        (1_000..=60_000).contains(&cli.max_ttl_ms),
        "max TTL must be between 1000 and 60000 milliseconds"
    );
    validate_private_parent(&cli.socket)?;
    ensure!(
        fs::symlink_metadata(&cli.socket).is_err(),
        "signer socket path already exists"
    );
    validate_private_file("keystore", &cli.keystore, false)?;
    validate_private_file("cast executable", &cli.cast_bin, true)?;

    Ok(Policy {
        key_id: cli.key_id.clone(),
        signer_address: cli.signer_address.clone(),
        allowed_asset: cli.allowed_asset,
        allowed_side: cli.allowed_side,
        max_notional: cli.max_notional,
        max_ttl_ms: cli.max_ttl_ms,
        cast_bin: cli.cast_bin.clone(),
        keystore: cli.keystore.clone(),
    })
}

fn validate_private_parent(socket: &Path) -> Result<()> {
    ensure!(socket.is_absolute(), "signer socket path must be absolute");
    let parent = socket
        .parent()
        .context("signer socket must have a parent directory")?;
    let metadata =
        fs::symlink_metadata(parent).context("cannot inspect signer socket directory")?;
    ensure!(
        metadata.is_dir(),
        "signer socket parent must be a directory"
    );
    ensure!(
        !metadata.file_type().is_symlink(),
        "signer socket parent must not be a symlink"
    );
    ensure!(
        has_no_group_or_other_permissions(metadata.permissions().mode()),
        "signer socket directory must not be accessible to group or other users"
    );
    Ok(())
}

fn validate_private_file(label: &str, path: &Path, executable: bool) -> Result<()> {
    ensure!(path.is_absolute(), "{label} path must be absolute");
    let metadata = fs::symlink_metadata(path).with_context(|| format!("cannot inspect {label}"))?;
    ensure!(metadata.is_file(), "{label} must be a regular file");
    ensure!(
        !metadata.file_type().is_symlink(),
        "{label} must not be a symlink"
    );
    if executable {
        ensure!(
            metadata.permissions().mode() & 0o111 != 0,
            "{label} is not executable"
        );
    } else {
        ensure!(
            has_no_group_or_other_permissions(metadata.permissions().mode()),
            "encrypted keystore must not be accessible to group or other users"
        );
    }
    Ok(())
}

const fn has_no_group_or_other_permissions(mode: u32) -> bool {
    mode.trailing_zeros() >= 6
}

fn bind_private_socket(path: &Path) -> Result<UnixListener> {
    let listener = UnixListener::bind(path).context("could not bind signer socket")?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("could not restrict signer socket permissions")?;
    let metadata = fs::symlink_metadata(path).context("cannot inspect bound signer socket")?;
    ensure!(
        metadata.file_type().is_socket() && !metadata.file_type().is_symlink(),
        "bound signer path is not a Unix-domain socket"
    );
    Ok(listener)
}

fn read_request(stream: &mut UnixStream) -> Result<SignerRequest> {
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .context("could not set signer read timeout")?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .context("could not set signer write timeout")?;
    let mut bytes = Vec::new();
    stream
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("could not read signer request")?;
    ensure!(
        !bytes.is_empty() && bytes.len() as u64 <= MAX_REQUEST_BYTES,
        "signer request is empty or oversized"
    );
    serde_json::from_slice(&bytes).context("signer request is not strict schema-v1 JSON")
}

fn validate_request(
    request: &SignerRequest,
    policy: &Policy,
    phase: &Phase,
) -> Result<ValidatedRequest> {
    ensure!(
        request.schema_version == SIGNER_PROTOCOL_VERSION,
        "unsupported signer protocol version"
    );
    ensure!(
        request.network == ExecutionNetwork::Testnet,
        "signer accepts only typed testnet requests"
    );
    ensure!(
        request.key_id == policy.key_id && request.signer_address == policy.signer_address,
        "signer request identity does not match pinned policy"
    );
    validate_request_time(request, policy.max_ttl_ms)?;

    let next_phase = match phase {
        Phase::Order => validate_order_action(&request.action, policy)?,
        Phase::Cancel { cloid } => validate_cancel_action(&request.action, policy, cloid)?,
        Phase::Complete => bail!("one-shot signer session is already complete"),
    };
    let action = serde_json::from_value(request.action.clone())
        .context("request action is not supported by the pinned Hyperliquid SDK")?;
    Ok(ValidatedRequest { action, next_phase })
}

fn validate_request_time(request: &SignerRequest, max_ttl_ms: u64) -> Result<()> {
    let now = unix_time_ms()?;
    ensure!(
        request.nonce.abs_diff(now) <= MAX_CLOCK_SKEW_MS,
        "signer request nonce is outside the allowed clock window"
    );
    ensure!(
        request.expires_after > now && request.expires_after > request.nonce,
        "signer request is expired or has an invalid expiry"
    );
    ensure!(
        request.expires_after.saturating_sub(request.nonce) <= max_ttl_ms,
        "signer request TTL exceeds pinned policy"
    );
    Ok(())
}

fn validate_order_action(action: &Value, policy: &Policy) -> Result<Phase> {
    let object = strict_object(action, &["type", "orders", "grouping"])?;
    ensure!(
        object.get("type").and_then(Value::as_str) == Some("order"),
        "first action must be order"
    );
    ensure!(
        object.get("grouping").and_then(Value::as_str) == Some("na"),
        "order grouping must be na"
    );
    let orders = object
        .get("orders")
        .and_then(Value::as_array)
        .context("orders must be an array")?;
    ensure!(orders.len() == 1, "signer accepts exactly one order");
    let order = strict_object(&orders[0], &["a", "b", "p", "s", "r", "t", "c"])?;
    validate_asset(order.get("a"), policy.allowed_asset)?;
    let is_buy = order
        .get("b")
        .and_then(Value::as_bool)
        .context("order side must be boolean")?;
    ensure!(
        is_buy == matches!(policy.allowed_side, AllowedSide::Buy),
        "order side does not match pinned policy"
    );
    ensure!(
        order.get("r").and_then(Value::as_bool) == Some(false),
        "test order must not be reduce-only"
    );
    validate_gtc(order.get("t"))?;

    let price = parse_positive_decimal(order.get("p"), "order price")?;
    let size = parse_positive_decimal(order.get("s"), "order size")?;
    let notional = price.checked_mul(size).context("order notional overflow")?;
    ensure!(
        notional <= policy.max_notional,
        "order notional exceeds signer policy"
    );
    let cloid = order
        .get("c")
        .and_then(Value::as_str)
        .context("order must include cloid")?;
    validate_cloid(cloid)?;
    Ok(Phase::Cancel {
        cloid: cloid.to_owned(),
    })
}

fn validate_cancel_action(action: &Value, policy: &Policy, expected_cloid: &str) -> Result<Phase> {
    let object = strict_object(action, &["type", "cancels"])?;
    ensure!(
        object.get("type").and_then(Value::as_str) == Some("cancelByCloid"),
        "second action must cancel by cloid"
    );
    let cancels = object
        .get("cancels")
        .and_then(Value::as_array)
        .context("cancels must be an array")?;
    ensure!(cancels.len() == 1, "signer accepts exactly one cancel");
    let cancel = strict_object(&cancels[0], &["asset", "cloid"])?;
    validate_asset(cancel.get("asset"), policy.allowed_asset)?;
    ensure!(
        cancel.get("cloid").and_then(Value::as_str) == Some(expected_cloid),
        "cancel cloid does not match the signed order"
    );
    Ok(Phase::Complete)
}

fn validate_gtc(value: Option<&Value>) -> Result<()> {
    let time = strict_object(
        value.context("order time-in-force is required")?,
        &["limit"],
    )?;
    let limit = strict_object(
        time.get("limit").context("limit policy is required")?,
        &["tif"],
    )?;
    ensure!(
        limit.get("tif").and_then(Value::as_str) == Some("Gtc"),
        "signer accepts only GTC limit orders"
    );
    Ok(())
}

fn validate_asset(value: Option<&Value>, allowed: u32) -> Result<()> {
    let asset = value
        .and_then(Value::as_u64)
        .context("asset ID must be an unsigned integer")?;
    ensure!(
        asset == u64::from(allowed),
        "asset ID does not match pinned policy"
    );
    Ok(())
}

fn parse_positive_decimal(value: Option<&Value>, label: &str) -> Result<Decimal> {
    let text = value
        .and_then(Value::as_str)
        .with_context(|| format!("{label} must be a string"))?;
    let decimal = Decimal::from_str(text).with_context(|| format!("{label} is invalid"))?;
    ensure!(decimal > Decimal::ZERO, "{label} must be positive");
    Ok(decimal)
}

fn strict_object<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let object = value
        .as_object()
        .context("action component must be an object")?;
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = keys.iter().copied().collect();
    ensure!(
        actual == expected,
        "action component contains missing or unexpected fields"
    );
    Ok(object)
}

fn sign_request(
    request: &SignerRequest,
    action: &Action,
    policy: &Policy,
    shutdown: &AtomicBool,
) -> Result<Value> {
    let expires_ms = i64::try_from(request.expires_after).context("expiry exceeds i64")?;
    let expires = DateTime::from_timestamp_millis(expires_ms).context("expiry is invalid")?;
    let prehash = action
        .prehash(request.nonce, None, Some(expires), Chain::Testnet)
        .context("could not compute Hyperliquid testnet signing prehash")?;

    let mut child = Command::new(&policy.cast_bin)
        .args(["wallet", "sign", "--no-hash", "--keystore"])
        .arg(&policy.keystore)
        .arg(prehash.to_string())
        .env_remove("ETH_PASSWORD")
        .env_remove("CAST_UNSAFE_PASSWORD")
        .env_remove("ETH_KEYSTORE")
        .env_remove("ETH_KEYSTORE_ACCOUNT")
        .env_remove("ETH_FROM")
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("could not invoke reviewed cast executable")?;
    let status = wait_for_cast(&mut child, shutdown)?;
    let mut signature_bytes = Vec::new();
    child
        .stdout
        .take()
        .context("cast signature output was not captured")?
        .read_to_end(&mut signature_bytes)
        .context("could not read cast signature output")?;
    ensure!(status.success(), "cast refused to sign the bounded request");
    let signature_text =
        String::from_utf8(signature_bytes).context("cast signature output is not UTF-8")?;
    let signature_text = signature_text.trim();
    ensure!(
        signature_text.split_whitespace().count() == 1,
        "cast returned an unexpected signature response"
    );
    let signature = AlloySignature::from_str(signature_text)
        .context("cast returned an invalid Ethereum signature")?;
    let recovered = signature
        .recover_address_from_prehash(&prehash)
        .context("could not recover cast signature address")?
        .to_string()
        .to_ascii_lowercase();
    ensure!(
        recovered == policy.signer_address,
        "cast signature did not recover the pinned agent address"
    );

    Ok(json!({
        "action": action,
        "nonce": request.nonce,
        "signature": {
            "r": fixed_width_scalar(signature.r()),
            "s": fixed_width_scalar(signature.s()),
            "v": u64::from(signature.v_byte()),
        },
        "vaultAddress": null,
        "expiresAfter": request.expires_after,
    }))
}

fn wait_for_cast(child: &mut Child, shutdown: &AtomicBool) -> Result<ExitStatus> {
    loop {
        if let Some(status) = child
            .try_wait()
            .context("could not poll reviewed cast executable")?
        {
            return Ok(status);
        }
        if shutdown.load(Ordering::Relaxed) {
            let kill_result = child.kill();
            let wait_result = child.wait();
            kill_result.context("could not terminate interrupted cast executable")?;
            wait_result.context("could not reap interrupted cast executable")?;
            bail!("signer interrupted while cast was running");
        }
        thread::sleep(ACCEPT_POLL_INTERVAL);
    }
}

fn fixed_width_scalar(value: U256) -> String {
    format!("0x{value:064x}")
}

fn write_response(
    stream: &mut UnixStream,
    request: &SignerRequest,
    signed_request: &Value,
) -> Result<()> {
    let response = SignerResponse {
        schema_version: SIGNER_PROTOCOL_VERSION,
        network: ExecutionNetwork::Testnet,
        key_id: &request.key_id,
        signer_address: &request.signer_address,
        signed_request: signed_request.clone(),
    };
    serde_json::to_writer(&mut *stream, &response).context("could not write signer response")?;
    stream
        .write_all(b"\n")
        .context("could not terminate signer response")?;
    stream.flush().context("could not flush signer response")?;
    Ok(())
}

fn unix_time_ms() -> Result<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock predates Unix epoch")?;
    u64::try_from(elapsed.as_millis()).context("Unix timestamp exceeds u64")
}

fn validate_address(address: &str) -> Result<()> {
    ensure!(
        address.len() == 42
            && address.starts_with("0x")
            && address[2..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "signer address must be 0x plus 40 lowercase hexadecimal digits"
    );
    Ok(())
}

fn validate_alias(alias: &str) -> Result<()> {
    ensure!(
        !alias.is_empty()
            && alias.len() <= 128
            && alias.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/')
            }),
        "key ID must be a bounded non-secret alias"
    );
    Ok(())
}

fn validate_cloid(cloid: &str) -> Result<()> {
    ensure!(
        cloid.len() == 34
            && cloid.starts_with("0x")
            && cloid[2..].bytes().all(|byte| byte.is_ascii_hexdigit()),
        "cloid must be 0x plus 16 bytes of hexadecimal"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;
    use serde_json::json;
    use tempfile::tempdir;

    fn policy() -> Policy {
        Policy {
            key_id: "foundry/testnet-agent".to_owned(),
            signer_address: "0x1111111111111111111111111111111111111111".to_owned(),
            allowed_asset: 3,
            allowed_side: AllowedSide::Buy,
            max_notional: Decimal::from(12),
            max_ttl_ms: 60_000,
            cast_bin: PathBuf::from("/unused/cast"),
            keystore: PathBuf::from("/unused/keystore"),
        }
    }

    fn order_action() -> Value {
        json!({
            "type": "order",
            "orders": [{
                "a": 3,
                "b": true,
                "p": "78000",
                "s": "0.00015",
                "r": false,
                "t": {"limit": {"tif": "Gtc"}},
                "c": "0x11111111111111111111111111111111"
            }],
            "grouping": "na"
        })
    }

    #[test]
    fn accepts_one_bounded_order_then_its_exact_cancel() {
        let policy = policy();
        let next = validate_order_action(&order_action(), &policy).unwrap();
        assert_eq!(
            next,
            Phase::Cancel {
                cloid: "0x11111111111111111111111111111111".to_owned()
            }
        );
        let cancel = json!({
            "type": "cancelByCloid",
            "cancels": [{
                "asset": 3,
                "cloid": "0x11111111111111111111111111111111"
            }]
        });
        assert_eq!(
            validate_cancel_action(&cancel, &policy, "0x11111111111111111111111111111111").unwrap(),
            Phase::Complete
        );
    }

    #[test]
    fn rejects_wrong_asset_side_cloid_and_excess_notional() {
        let policy = policy();
        let mut action = order_action();
        action["orders"][0]["a"] = json!(0);
        assert!(validate_order_action(&action, &policy).is_err());

        let mut action = order_action();
        action["orders"][0]["b"] = json!(false);
        assert!(validate_order_action(&action, &policy).is_err());

        let mut action = order_action();
        action["orders"][0]["s"] = json!("1");
        assert!(validate_order_action(&action, &policy).is_err());

        let cancel = json!({
            "type": "cancelByCloid",
            "cancels": [{
                "asset": 3,
                "cloid": "0x22222222222222222222222222222222"
            }]
        });
        assert!(
            validate_cancel_action(&cancel, &policy, "0x11111111111111111111111111111111").is_err()
        );
    }

    #[test]
    fn rejects_unknown_action_fields() {
        let policy = policy();
        let mut action = order_action();
        action["unexpected"] = json!(true);
        assert!(validate_order_action(&action, &policy).is_err());
    }

    #[test]
    fn pinned_sdk_accepts_and_hashes_exact_wire_actions() {
        let expires = chrono::Utc.timestamp_millis_opt(20_000).unwrap();
        let order: Action = serde_json::from_value(order_action()).unwrap();
        let order_hash = order
            .prehash(10_000, None, Some(expires), Chain::Testnet)
            .unwrap();
        assert_ne!(order_hash, alloy::primitives::B256::ZERO);

        let cancel: Action = serde_json::from_value(json!({
            "type": "cancelByCloid",
            "cancels": [{
                "asset": 3,
                "cloid": "0x11111111111111111111111111111111"
            }]
        }))
        .unwrap();
        let cancel_hash = cancel
            .prehash(10_001, None, Some(expires), Chain::Testnet)
            .unwrap();
        assert_ne!(cancel_hash, alloy::primitives::B256::ZERO);
        assert_ne!(order_hash, cancel_hash);
    }

    #[test]
    fn signature_scalars_are_always_encoded_as_exactly_32_bytes() {
        let encoded = fixed_width_scalar(U256::from(1));
        assert_eq!(encoded.len(), 66);
        assert_eq!(
            encoded,
            "0x0000000000000000000000000000000000000000000000000000000000000001"
        );
    }

    #[test]
    fn shutdown_while_waiting_removes_the_one_shot_socket() {
        let directory = tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.path().join("signer.sock");
        {
            let listener = bind_private_socket(&path).unwrap();
            let _guard = SocketGuard(path.clone());
            let shutdown = install_shutdown_handlers().unwrap();
            signal_hook::low_level::raise(SIGTERM).unwrap();

            assert!(
                accept_until_shutdown(&listener, shutdown.as_ref())
                    .unwrap()
                    .is_none()
            );
            assert!(path.exists());
        }
        assert!(!path.exists());
    }

    #[test]
    fn shutdown_terminates_and_reaps_an_interactive_cast_child() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let shutdown = AtomicBool::new(true);

        let error = wait_for_cast(&mut child, &shutdown).unwrap_err();
        assert!(error.to_string().contains("interrupted"));
        assert!(child.try_wait().unwrap().is_some());
    }
}
