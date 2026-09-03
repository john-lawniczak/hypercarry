use crate::evidence::{SessionEvidence, hash_file};
use anyhow::{Context, Result, bail, ensure};
use hypercarry_execution::{
    ClientOrderId, ExecutionMode, JournalEventKind, OrderState, OrderStateMachine, Side,
    ValidatedOrder, read_journal,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::NamedTempFile;

const REVIEW_SCHEMA_VERSION: u32 = 1;
const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const FINAL_STATE_CHECK: &str = "official-order-status-open-orders-user-fills-v1";

/// Exact declaration required from an independent human reviewer.
pub const REVIEW_ACKNOWLEDGEMENT: &str = "I INDEPENDENTLY REVIEWED THIS HYPERCARRY TESTNET SESSION AND FOUND NO SECRETS OR UNMANAGED ORDERS";

/// Immutable inputs used to review one completed credentialed session.
#[derive(Debug, Clone)]
pub struct ReviewRequest {
    /// Path to the immutable harness session evidence.
    pub evidence_path: PathBuf,
    /// Path to the exact operator configuration hashed by the evidence.
    pub config_path: PathBuf,
    /// Path to the durable execution journal for the session.
    pub journal_path: PathBuf,
    /// Path to the artifact manifest binding the reviewed hashes.
    pub manifest_path: PathBuf,
    /// Path to the official account-wide open-orders check.
    pub final_open_orders_path: PathBuf,
    /// Path to the official terminal order-status check.
    pub final_order_status_path: PathBuf,
    /// Path to the official user-fills check.
    pub final_user_fills_path: PathBuf,
    /// Destination for the non-overwriting attestation file.
    pub output_path: PathBuf,
    /// Identity of the independent human reviewer.
    pub reviewer: String,
    /// Exact reviewer acknowledgement text.
    pub acknowledgement: String,
}

/// Separate reviewer-owned attestation for an immutable harness evidence file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAttestation {
    /// Version of the attestation contract.
    pub schema_version: u32,
    /// Identifier of the reviewed session.
    pub session_id: String,
    /// Source commit the session ran at.
    pub commit: String,
    /// Session start time, unix milliseconds.
    pub started_at_ms: i64,
    /// Session end time, unix milliseconds.
    pub ended_at_ms: i64,
    /// Time the review was performed, unix milliseconds.
    pub reviewed_at_ms: i64,
    /// Identity of the independent human reviewer.
    pub reviewer: String,
    /// Whether the reviewer confirmed organizational independence.
    pub independent_review: bool,
    /// Whether a manual secret scan of the artifacts was completed.
    pub manual_secret_scan_completed: bool,
    /// Version of the reviewer acknowledgement text used.
    pub reviewer_acknowledgement_version: u32,
    /// Name of the final-state check methodology applied.
    pub final_state_check: String,
    /// Whether the reviewed session ended with no orphaned or unmanaged state.
    pub lifecycle_clean: bool,
    /// Final lifecycle state cross-checked against the official API.
    pub final_state: OrderState,
    /// Durable client order identifier reviewed.
    pub client_order_id: String,
    /// Venue-assigned order identifier reviewed.
    pub venue_order_id: u64,
    /// Cumulative filled quantity cross-checked against the official API.
    #[serde(with = "rust_decimal::serde::str")]
    pub cumulative_filled: Decimal,
    /// Independent account-wide open-order count observed at close.
    pub independent_open_order_count: usize,
    /// Fills matched between the journal and the official API.
    pub matching_fill_count: usize,
    /// Orphaned orders found during review.
    pub orphaned_order_count: u32,
    /// Unmanaged orders found during review.
    pub unmanaged_order_count: u32,
    /// Submissions whose outcome remained unresolved.
    pub unresolved_submission_count: u32,
    /// Risk policy bypasses found during review.
    pub policy_bypass_count: u32,
    /// Secret leaks found during the manual scan.
    pub secret_leak_count: u32,
    /// Names of fault-injection exercises reviewed.
    pub faults_reviewed: Vec<String>,
    /// SHA-256 digest of the reviewed evidence file.
    pub evidence_sha256: String,
    /// SHA-256 digest of the reviewed configuration file.
    pub config_sha256: String,
    /// SHA-256 digest of the reviewed journal file.
    pub journal_sha256: String,
    /// SHA-256 digest of the reviewed artifact manifest.
    pub manifest_sha256: String,
    /// SHA-256 digest of the reviewed operator executable.
    pub operator_executable_sha256: String,
    /// SHA-256 digest of the reviewed signer executable.
    pub signer_executable_sha256: String,
    /// SHA-256 digest of the official open-orders check file.
    pub final_open_orders_sha256: String,
    /// SHA-256 digest of the official order-status check file.
    pub final_order_status_sha256: String,
    /// SHA-256 digest of the official user-fills check file.
    pub final_user_fills_sha256: String,
}

/// Completed review plus the digest of its non-overwriting attestation file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewOutcome {
    /// The written attestation.
    pub attestation: ReviewAttestation,
    /// SHA-256 digest of the attestation file.
    pub attestation_sha256: String,
}

struct ReviewDigests {
    evidence: String,
    config: String,
    journal: String,
    manifest: String,
    operator_executable: String,
    signer_executable: String,
    final_open_orders: String,
    final_order_status: String,
    final_user_fills: String,
}

/// Verifies immutable session artifacts and writes a separate review attestation.
///
/// This function performs no network I/O and never edits its inputs.
///
/// # Errors
///
/// Returns an error for an invalid declaration, mutable or inconsistent
/// artifacts, a non-clean lifecycle, failed independent final-state checks, or
/// an existing output path.
pub fn review_session(request: &ReviewRequest) -> Result<ReviewOutcome> {
    review_session_at(request, unix_time_ms()?)
}

fn review_session_at(request: &ReviewRequest, reviewed_at_ms: i64) -> Result<ReviewOutcome> {
    ensure!(
        request.acknowledgement == REVIEW_ACKNOWLEDGEMENT,
        "review requires exact acknowledgement `{REVIEW_ACKNOWLEDGEMENT}`"
    );
    validate_reviewer(&request.reviewer)?;
    ensure!(reviewed_at_ms >= 0, "review timestamp must not be negative");

    validate_request_paths(request)?;

    let evidence: SessionEvidence = read_strict_json(&request.evidence_path)?;
    validate_candidate_evidence(&evidence, reviewed_at_ms, &request.reviewer)?;
    let digests = validate_artifact_bindings(request, &evidence)?;

    let (order, venue_order_id) = validate_journal(&request.journal_path, &evidence)?;
    let independent_open_order_count = validate_open_orders(&request.final_open_orders_path)?;
    let status = validate_order_status(
        &request.final_order_status_path,
        &evidence,
        &order,
        venue_order_id,
    )?;
    let matching_fill_count =
        validate_user_fills(&request.final_user_fills_path, &evidence, venue_order_id)?;
    validate_artifacts_unchanged(request, &digests)?;

    let attestation = ReviewAttestation {
        schema_version: REVIEW_SCHEMA_VERSION,
        session_id: evidence.session_id,
        commit: evidence.commit,
        started_at_ms: evidence.started_at_ms,
        ended_at_ms: evidence.ended_at_ms,
        reviewed_at_ms,
        reviewer: request.reviewer.clone(),
        independent_review: true,
        manual_secret_scan_completed: true,
        reviewer_acknowledgement_version: 1,
        final_state_check: FINAL_STATE_CHECK.to_owned(),
        lifecycle_clean: true,
        final_state: evidence.final_state,
        client_order_id: evidence.client_order_id,
        venue_order_id,
        cumulative_filled: evidence.cumulative_filled,
        independent_open_order_count,
        matching_fill_count,
        orphaned_order_count: 0,
        unmanaged_order_count: 0,
        unresolved_submission_count: evidence.unresolved_submission_count,
        policy_bypass_count: evidence.policy_bypass_count,
        secret_leak_count: 0,
        faults_reviewed: evidence.faults_injected,
        evidence_sha256: digests.evidence,
        config_sha256: digests.config,
        journal_sha256: digests.journal,
        manifest_sha256: digests.manifest,
        operator_executable_sha256: digests.operator_executable,
        signer_executable_sha256: digests.signer_executable,
        final_open_orders_sha256: digests.final_open_orders,
        final_order_status_sha256: digests.final_order_status,
        final_user_fills_sha256: digests.final_user_fills,
    };
    ensure!(
        status == attestation.final_state,
        "final-state review mismatch"
    );
    write_new_json(&request.output_path, &attestation)?;
    let attestation_sha256 = hash_file(&request.output_path)?;
    Ok(ReviewOutcome {
        attestation,
        attestation_sha256,
    })
}

fn validate_request_paths(request: &ReviewRequest) -> Result<()> {
    let inputs = [
        (&request.evidence_path, MAX_JSON_BYTES),
        (&request.config_path, MAX_JSON_BYTES),
        (&request.journal_path, MAX_JSON_BYTES),
        (&request.manifest_path, MAX_MANIFEST_BYTES),
        (&request.final_open_orders_path, MAX_JSON_BYTES),
        (&request.final_order_status_path, MAX_JSON_BYTES),
        (&request.final_user_fills_path, MAX_JSON_BYTES),
    ];
    let mut unique_paths = BTreeSet::new();
    for (path, max_bytes) in inputs {
        validate_private_regular_file(path, max_bytes)?;
        ensure!(
            unique_paths.insert(path.clone()),
            "review input paths must be distinct"
        );
    }
    validate_new_output(&request.output_path)
}

fn validate_artifact_bindings(
    request: &ReviewRequest,
    evidence: &SessionEvidence,
) -> Result<ReviewDigests> {
    let digests = ReviewDigests {
        evidence: hash_file(&request.evidence_path)?,
        config: hash_file(&request.config_path)?,
        journal: hash_file(&request.journal_path)?,
        manifest: hash_file(&request.manifest_path)?,
        operator_executable: evidence.operator_executable_sha256.clone(),
        signer_executable: String::new(),
        final_open_orders: hash_file(&request.final_open_orders_path)?,
        final_order_status: hash_file(&request.final_order_status_path)?,
        final_user_fills: hash_file(&request.final_user_fills_path)?,
    };
    ensure!(
        digests.config == evidence.config_sha256,
        "config digest does not match immutable session evidence"
    );
    ensure!(
        digests.journal == evidence.journal_sha256,
        "journal digest does not match immutable session evidence"
    );

    let manifest = read_manifest(&request.manifest_path)?;
    for (name, actual) in [
        ("session.evidence.json", digests.evidence.as_str()),
        ("operator-run.json", digests.config.as_str()),
        ("session.journal.jsonl", digests.journal.as_str()),
        (
            "independent-final-open-orders.json",
            digests.final_open_orders.as_str(),
        ),
        (
            "independent-final-order-status.json",
            digests.final_order_status.as_str(),
        ),
        (
            "independent-final-user-fills.json",
            digests.final_user_fills.as_str(),
        ),
        (
            "hypercarry-testnet-operator",
            digests.operator_executable.as_str(),
        ),
    ] {
        require_manifest_digest(&manifest, name, actual)?;
    }
    let signer_executable = manifest
        .get("hypercarry-foundry-testnet-signer")
        .context("artifact manifest lacks the reviewed signer executable")?
        .clone();
    Ok(ReviewDigests {
        signer_executable,
        ..digests
    })
}

fn validate_artifacts_unchanged(request: &ReviewRequest, digests: &ReviewDigests) -> Result<()> {
    for (path, expected) in [
        (&request.evidence_path, digests.evidence.as_str()),
        (&request.config_path, digests.config.as_str()),
        (&request.journal_path, digests.journal.as_str()),
        (&request.manifest_path, digests.manifest.as_str()),
        (
            &request.final_open_orders_path,
            digests.final_open_orders.as_str(),
        ),
        (
            &request.final_order_status_path,
            digests.final_order_status.as_str(),
        ),
        (
            &request.final_user_fills_path,
            digests.final_user_fills.as_str(),
        ),
    ] {
        ensure!(
            hash_file(path)? == expected,
            "review artifact changed while it was being validated: {}",
            path.display()
        );
    }
    Ok(())
}

fn validate_candidate_evidence(
    evidence: &SessionEvidence,
    reviewed_at_ms: i64,
    reviewer: &str,
) -> Result<()> {
    ensure!(
        evidence.schema_version == 1,
        "unsupported session evidence schema"
    );
    ensure!(
        evidence.network == hypercarry_execution::ExecutionNetwork::Testnet,
        "review accepts only typed testnet evidence"
    );
    ensure!(
        evidence.started_at_ms >= 0
            && evidence.ended_at_ms > evidence.started_at_ms
            && reviewed_at_ms >= evidence.ended_at_ms,
        "session/review timestamps are invalid"
    );
    ensure!(
        evidence.lifecycle_clean,
        "session evidence is not lifecycle-clean"
    );
    ensure!(
        matches!(
            evidence.final_state,
            OrderState::Cancelled | OrderState::Filled
        ),
        "session evidence is not terminal-clean"
    );
    ensure!(
        evidence.independent_open_order_count == 0
            && evidence.unresolved_submission_count == 0
            && evidence.policy_bypass_count == 0,
        "session evidence reports an operational exception"
    );
    ensure!(
        !evidence.manual_secret_scan_completed && evidence.reviewer.is_none(),
        "input must be the immutable unreviewed harness evidence"
    );
    ensure!(
        reviewer != evidence.signer_alias
            && reviewer != evidence.account_address
            && reviewer != evidence.authorized_signer_address
            && reviewer != evidence.session_id,
        "reviewer identity is not independent from session identities"
    );
    validate_hex("commit", &evidence.commit, 40)?;
    validate_hex("config digest", &evidence.config_sha256, 64)?;
    validate_hex("journal digest", &evidence.journal_sha256, 64)?;
    validate_hex(
        "operator executable digest",
        &evidence.operator_executable_sha256,
        64,
    )?;
    Ok(())
}

fn validate_journal(path: &Path, evidence: &SessionEvidence) -> Result<(ValidatedOrder, u64)> {
    let events = read_journal(path).context("could not validate immutable session journal")?;
    ensure!(!events.is_empty(), "session journal is empty");
    let exact_orders = events
        .iter()
        .filter_map(|event| match &event.event {
            JournalEventKind::ExactAction {
                mode: ExecutionMode::Testnet,
                order,
            } => Some(order),
            _ => None,
        })
        .collect::<Vec<_>>();
    ensure!(
        exact_orders.len() == 1,
        "journal must contain one exact testnet action"
    );
    let order = exact_orders[0].clone();
    ensure!(
        order.market == evidence.market
            && order.side == evidence.side
            && order.quantity == evidence.requested_quantity
            && order.limit_price == evidence.requested_limit_price,
        "journal exact action does not match session evidence"
    );
    ensure!(
        events.iter().any(|event| matches!(
            &event.event,
            JournalEventKind::RiskDecisionRecorded { allowed: true, .. }
        )),
        "journal lacks an allowed risk decision"
    );
    ensure!(
        !events.iter().any(|event| matches!(
            &event.event,
            JournalEventKind::RiskRejected { .. } | JournalEventKind::VenueRejected { .. }
        )),
        "journal contains a rejection"
    );
    let client_order_id = ClientOrderId::from_str(&evidence.client_order_id)
        .context("evidence client order ID is invalid")?;
    let recovered =
        OrderStateMachine::recover_from_journal(order.clone(), client_order_id, &events)
            .context("journal lifecycle does not replay")?;
    ensure!(
        recovered.state() == evidence.final_state
            && recovered.cumulative_filled() == evidence.cumulative_filled,
        "journal terminal state does not match session evidence"
    );
    let venue_order_id = recovered
        .venue_order_id()
        .context("journal terminal state lacks a venue order ID")?;
    Ok((order, venue_order_id))
}

fn validate_open_orders(path: &Path) -> Result<usize> {
    let value: Value = read_strict_json(path)?;
    let orders = value
        .as_array()
        .context("independent open-orders artifact must be an array")?;
    ensure!(
        orders.iter().all(Value::is_object),
        "independent open-orders artifact contains a non-object"
    );
    ensure!(
        orders.is_empty(),
        "independent final check found open orders"
    );
    Ok(orders.len())
}

fn validate_order_status(
    path: &Path,
    evidence: &SessionEvidence,
    order: &ValidatedOrder,
    venue_order_id: u64,
) -> Result<OrderState> {
    let value: Value = read_strict_json(path)?;
    ensure!(
        value.get("status").and_then(Value::as_str) == Some("order"),
        "independent order-status artifact is not an order envelope"
    );
    let record = value
        .get("order")
        .context("independent order-status artifact lacks its record")?;
    let status = record
        .get("status")
        .and_then(Value::as_str)
        .context("independent order-status record lacks terminal status")?;
    let reviewed_state = match status {
        "filled" => OrderState::Filled,
        "canceled"
        | "marginCanceled"
        | "vaultWithdrawalCanceled"
        | "openInterestCapCanceled"
        | "selfTradeCanceled"
        | "reduceOnlyCanceled"
        | "siblingFilledCanceled"
        | "delistedCanceled"
        | "scheduledCancel" => OrderState::Cancelled,
        _ => bail!("independent order status is not terminal-clean: {status}"),
    };
    ensure!(
        reviewed_state == evidence.final_state,
        "independent terminal state differs"
    );
    let wire = record
        .get("order")
        .context("independent order-status record lacks order fields")?;
    ensure!(
        wire.get("oid").and_then(Value::as_u64) == Some(venue_order_id)
            && wire.get("cloid").and_then(Value::as_str) == Some(evidence.client_order_id.as_str())
            && wire.get("coin").and_then(Value::as_str) == Some(evidence.market.as_str()),
        "independent order identity does not match evidence"
    );
    let expected_side = match evidence.side {
        Side::Buy => "B",
        Side::Sell => "A",
    };
    ensure!(
        wire.get("side").and_then(Value::as_str) == Some(expected_side),
        "independent order side does not match evidence"
    );
    let limit_price = parse_decimal_field(wire, "limitPx", "independent limit price")?;
    let original_size = parse_decimal_field(wire, "origSz", "independent original size")?;
    ensure!(
        limit_price == order.limit_price && original_size == order.quantity,
        "independent order values do not match journal"
    );
    let status_timestamp = record
        .get("statusTimestamp")
        .and_then(Value::as_i64)
        .context("independent order-status record lacks status timestamp")?;
    ensure!(
        status_timestamp >= evidence.started_at_ms && status_timestamp <= evidence.ended_at_ms,
        "independent terminal timestamp is outside the session interval"
    );
    Ok(reviewed_state)
}

fn validate_user_fills(
    path: &Path,
    evidence: &SessionEvidence,
    venue_order_id: u64,
) -> Result<usize> {
    let value: Value = read_strict_json(path)?;
    let fills = value
        .as_array()
        .context("independent user-fills artifact must be an array")?;
    let mut matching_count = 0_usize;
    let mut matching_quantity = Decimal::ZERO;
    let mut trade_ids = BTreeSet::new();
    for fill in fills {
        let fill_order_id = fill.get("oid").and_then(Value::as_u64);
        let fill_client_order_id = fill.get("cloid").and_then(Value::as_str);
        let matches_order = fill_order_id == Some(venue_order_id)
            || fill_client_order_id == Some(evidence.client_order_id.as_str());
        if !matches_order {
            continue;
        }
        ensure!(
            fill_order_id.is_none_or(|value| value == venue_order_id)
                && fill_client_order_id
                    .is_none_or(|value| value == evidence.client_order_id.as_str()),
            "matching fill contains a conflicting order identity"
        );
        let expected_side = match evidence.side {
            Side::Buy => "B",
            Side::Sell => "A",
        };
        ensure!(
            fill.get("coin").and_then(Value::as_str) == Some(evidence.market.as_str())
                && fill.get("side").and_then(Value::as_str) == Some(expected_side),
            "matching fill belongs to a different market or side"
        );
        let trade_id = fill
            .get("tid")
            .filter(|value| value.is_number() || value.is_string())
            .map(Value::to_string)
            .context("matching fill lacks a trade ID")?;
        ensure!(
            trade_ids.insert(trade_id),
            "matching fills contain a duplicate trade ID"
        );
        let fill_quantity = parse_decimal_field(fill, "sz", "matching fill size")?;
        ensure!(
            fill_quantity > Decimal::ZERO,
            "matching fill size is not positive"
        );
        let fill_timestamp = fill
            .get("time")
            .and_then(Value::as_i64)
            .context("matching fill lacks its timestamp")?;
        ensure!(
            fill_timestamp >= evidence.started_at_ms && fill_timestamp <= evidence.ended_at_ms,
            "matching fill timestamp is outside the session interval"
        );
        matching_quantity = matching_quantity
            .checked_add(fill_quantity)
            .context("matching fill quantity overflow")?;
        matching_count = matching_count
            .checked_add(1)
            .context("matching fill count overflow")?;
    }
    ensure!(
        matching_quantity == evidence.cumulative_filled,
        "matching fills do not equal evidence cumulative fill"
    );
    Ok(matching_count)
}

fn read_manifest(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = fs::read_to_string(path).context("could not read artifact manifest")?;
    let mut entries = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        ensure!(
            !line.trim().is_empty(),
            "artifact manifest contains a blank line"
        );
        let (digest, name) = line
            .split_once("  ")
            .with_context(|| format!("invalid artifact manifest line {}", index + 1))?;
        validate_hex("artifact digest", digest, 64)?;
        ensure!(
            !name.is_empty()
                && name.len() <= 128
                && !name.contains('/')
                && !name.contains('\\')
                && name.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                }),
            "artifact manifest contains an invalid basename"
        );
        ensure!(
            entries.insert(name.to_owned(), digest.to_owned()).is_none(),
            "artifact manifest contains a duplicate basename"
        );
    }
    ensure!(!entries.is_empty(), "artifact manifest is empty");
    Ok(entries)
}

fn require_manifest_digest(
    manifest: &BTreeMap<String, String>,
    name: &str,
    actual: &str,
) -> Result<()> {
    ensure!(
        manifest
            .get(name)
            .is_some_and(|expected| expected == actual),
        "artifact manifest digest mismatch for {name}"
    );
    Ok(())
}

fn validate_private_regular_file(path: &Path, max_bytes: u64) -> Result<()> {
    ensure!(path.is_absolute(), "review input paths must be absolute");
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect review input {}", path.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "review input must be a regular non-symlink file"
    );
    ensure!(
        metadata.len() > 0 && metadata.len() <= max_bytes,
        "review input is empty or oversized"
    );
    ensure!(
        metadata.permissions().mode().trailing_zeros() >= 6,
        "review input must not be accessible to group or other users"
    );
    validate_private_parent(path)
}

fn validate_new_output(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "review output path must be absolute");
    ensure!(
        fs::symlink_metadata(path).is_err(),
        "refusing to overwrite review attestation"
    );
    validate_private_parent(path)
}

fn validate_private_parent(path: &Path) -> Result<()> {
    let parent = path.parent().context("review path has no parent")?;
    let metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("cannot inspect review directory {}", parent.display()))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "review directory must be a non-symlink directory"
    );
    ensure!(
        metadata.permissions().mode().trailing_zeros() >= 6,
        "review directory must not be accessible to group or other users"
    );
    Ok(())
}

fn read_strict_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).context("could not read review JSON artifact")?;
    serde_json::from_slice(&bytes).context("review artifact is not valid strict JSON")
}

fn write_new_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("review output path has no parent")?;
    let mut temporary =
        NamedTempFile::new_in(parent).context("could not create review attestation temporary")?;
    serde_json::to_writer_pretty(&mut temporary, value)
        .context("could not serialize review attestation")?;
    temporary
        .write_all(b"\n")
        .context("could not terminate review attestation")?;
    temporary
        .as_file()
        .sync_all()
        .context("could not sync review attestation")?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)
        .context("could not atomically persist review attestation")?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .context("could not sync review attestation directory")?;
    Ok(())
}

fn parse_decimal_field(value: &Value, field: &str, label: &str) -> Result<Decimal> {
    let text = value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("{label} is missing"))?;
    Decimal::from_str(text).with_context(|| format!("{label} is invalid"))
}

fn validate_reviewer(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') }),
        "reviewer must be a bounded non-secret identifier"
    );
    Ok(())
}

fn validate_hex(label: &str, value: &str, length: usize) -> Result<()> {
    ensure!(
        value.len() == length
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "{label} must contain exactly {length} lowercase hexadecimal characters"
    );
    Ok(())
}

fn unix_time_ms() -> Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock predates Unix epoch")?;
    i64::try_from(elapsed.as_millis()).context("Unix timestamp exceeds i64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::SessionEvidence;
    use hypercarry_execution::{
        FileJournal, Journal, LifecycleTransition, OrderStateMachine, RiskDecision,
        TransitionOutcome,
    };
    use serde_json::json;
    use std::{fmt::Write as _, fs};
    use tempfile::{TempDir, tempdir};

    struct Fixture {
        _directory: TempDir,
        request: ReviewRequest,
    }

    #[test]
    fn review_binds_clean_immutable_artifacts_without_editing_evidence() {
        let fixture = fixture();
        let evidence_before = fs::read(&fixture.request.evidence_path).unwrap();

        let outcome = review_session_at(&fixture.request, 2_000).unwrap();

        assert_eq!(outcome.attestation.session_id, "session-1");
        assert_eq!(outcome.attestation.final_state, OrderState::Cancelled);
        assert_eq!(outcome.attestation.matching_fill_count, 0);
        assert!(outcome.attestation.manual_secret_scan_completed);
        assert_eq!(outcome.attestation_sha256.len(), 64);
        assert!(
            fs::metadata(&fixture.request.output_path)
                .unwrap()
                .permissions()
                .mode()
                .trailing_zeros()
                >= 6
        );
        assert_eq!(
            fs::read(&fixture.request.evidence_path).unwrap(),
            evidence_before
        );
        assert!(review_session_at(&fixture.request, 2_001).is_err());
    }

    #[test]
    fn review_rejects_tampered_journal_before_writing_attestation() {
        let fixture = fixture();
        fs::OpenOptions::new()
            .append(true)
            .open(&fixture.request.journal_path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();

        assert!(review_session_at(&fixture.request, 2_000).is_err());
        assert!(!fixture.request.output_path.exists());
    }

    #[test]
    fn review_requires_the_exact_human_acknowledgement() {
        let mut fixture = fixture();
        fixture.request.acknowledgement = "reviewed".to_owned();

        assert!(review_session_at(&fixture.request, 2_000).is_err());
        assert!(!fixture.request.output_path.exists());
    }

    #[test]
    fn review_rejects_a_conflicting_matching_fill_identity() {
        let fixture = fixture();
        write_private(
            &fixture.request.final_user_fills_path,
            serde_json::to_vec(&json!([{
                "coin": "BTC",
                "side": "B",
                "sz": "0.00015",
                "time": 1035,
                "oid": 42,
                "cloid": "0x00000000000000000000000000000000",
                "tid": 7
            }]))
            .unwrap()
            .as_slice(),
        );
        write_manifest_fixture(
            &fixture.request.manifest_path,
            &fixture.request.evidence_path,
            &fixture.request.config_path,
            &fixture.request.journal_path,
            &fixture.request.final_open_orders_path,
            &fixture.request.final_order_status_path,
            &fixture.request.final_user_fills_path,
        );

        assert!(review_session_at(&fixture.request, 2_000).is_err());
        assert!(!fixture.request.output_path.exists());
    }

    fn fixture() -> Fixture {
        let directory = tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let config_path = directory.path().join("operator-run.json");
        let journal_path = directory.path().join("session.journal.jsonl");
        let evidence_path = directory.path().join("session.evidence.json");
        let manifest_path = directory.path().join("artifact-sha256.txt");
        let final_open_orders_path = directory.path().join("independent-final-open-orders.json");
        let final_order_status_path = directory.path().join("independent-final-order-status.json");
        let final_user_fills_path = directory.path().join("independent-final-user-fills.json");
        let output_path = directory.path().join("review-attestation.json");

        write_private(&config_path, br#"{"schema_version":2}"#);
        let (order, client_order_id) = write_journal_fixture(&journal_path);
        write_final_state_fixtures(
            &final_open_orders_path,
            &final_order_status_path,
            &final_user_fills_path,
            client_order_id,
        );
        write_evidence_fixture(
            &evidence_path,
            &config_path,
            &journal_path,
            &order,
            client_order_id,
        );
        write_manifest_fixture(
            &manifest_path,
            &evidence_path,
            &config_path,
            &journal_path,
            &final_open_orders_path,
            &final_order_status_path,
            &final_user_fills_path,
        );

        Fixture {
            _directory: directory,
            request: ReviewRequest {
                evidence_path,
                config_path,
                journal_path,
                manifest_path,
                final_open_orders_path,
                final_order_status_path,
                final_user_fills_path,
                output_path,
                reviewer: "independent-reviewer".to_owned(),
                acknowledgement: REVIEW_ACKNOWLEDGEMENT.to_owned(),
            },
        }
    }

    fn write_journal_fixture(path: &Path) -> (ValidatedOrder, ClientOrderId) {
        let order = ValidatedOrder {
            schema_version: 1,
            correlation_id: "correlation-1".to_owned(),
            venue: "hyperliquid".to_owned(),
            market: "BTC".to_owned(),
            side: Side::Buy,
            quantity: Decimal::new(15, 5),
            limit_price: Decimal::from(78_738),
            created_at_ms: 1_000,
        };
        let client_order_id = ClientOrderId::derive(&order).unwrap();
        let mut journal = FileJournal::open(path).unwrap();
        journal
            .append(
                1_000,
                &order.correlation_id,
                RiskDecision::Allow {
                    code: "all_limits_satisfied".to_owned(),
                    reason: "fixture review".to_owned(),
                }
                .into(),
            )
            .unwrap();
        journal
            .append(
                1_000,
                &order.correlation_id,
                JournalEventKind::ExactAction {
                    mode: ExecutionMode::Testnet,
                    order: order.clone(),
                },
            )
            .unwrap();
        let mut machine = OrderStateMachine::new(order.clone(), client_order_id);
        for (state, at_ms, oid) in [
            (OrderState::SubmissionPending, 1_010, None),
            (OrderState::Open, 1_020, Some(42)),
            (OrderState::CancelPending, 1_030, Some(42)),
            (OrderState::Cancelled, 1_040, Some(42)),
        ] {
            let TransitionOutcome::Applied(transition) = machine
                .transition(state, at_ms, oid, Decimal::ZERO)
                .unwrap()
            else {
                panic!("fixture transition must apply");
            };
            journal
                .append(
                    transition.occurred_at_ms,
                    &order.correlation_id,
                    JournalEventKind::LifecycleTransition {
                        transition: LifecycleTransition { ..transition },
                    },
                )
                .unwrap();
        }
        drop(journal);
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        (order, client_order_id)
    }

    fn write_final_state_fixtures(
        open_orders_path: &Path,
        order_status_path: &Path,
        user_fills_path: &Path,
        client_order_id: ClientOrderId,
    ) {
        write_private(open_orders_path, b"[]");
        write_private(user_fills_path, b"[]");
        write_private(
            order_status_path,
            serde_json::to_vec(&json!({
                "status": "order",
                "order": {
                    "order": {
                        "coin": "BTC",
                        "side": "B",
                        "limitPx": "78738",
                        "origSz": "0.00015",
                        "oid": 42,
                        "cloid": client_order_id.to_string()
                    },
                    "status": "canceled",
                    "statusTimestamp": 1040
                }
            }))
            .unwrap()
            .as_slice(),
        );
    }

    fn write_evidence_fixture(
        evidence_path: &Path,
        config_path: &Path,
        journal_path: &Path,
        order: &ValidatedOrder,
        client_order_id: ClientOrderId,
    ) {
        let evidence = SessionEvidence {
            schema_version: 1,
            session_id: "session-1".to_owned(),
            commit: "a".repeat(40),
            started_at_ms: 1_000,
            ended_at_ms: 1_100,
            network: hypercarry_execution::ExecutionNetwork::Testnet,
            account_address: "0x1111111111111111111111111111111111111111".to_owned(),
            authorized_signer_address: "0x2222222222222222222222222222222222222222".to_owned(),
            signer_alias: "foundry/testnet-agent".to_owned(),
            market: "BTC".to_owned(),
            side: Side::Buy,
            requested_quantity: order.quantity,
            requested_limit_price: order.limit_price,
            client_order_id: client_order_id.to_string(),
            final_state: OrderState::Cancelled,
            cumulative_filled: Decimal::ZERO,
            independent_open_order_count: 0,
            unresolved_submission_count: 0,
            policy_bypass_count: 0,
            lifecycle_clean: true,
            config_sha256: hash_file(config_path).unwrap(),
            operator_executable_sha256: "d".repeat(64),
            journal_sha256: hash_file(journal_path).unwrap(),
            faults_injected: Vec::new(),
            manual_secret_scan_completed: false,
            reviewer: None,
        };
        evidence.write_new(evidence_path).unwrap();
    }

    fn write_manifest_fixture(
        manifest_path: &Path,
        evidence_path: &Path,
        config_path: &Path,
        journal_path: &Path,
        final_open_orders_path: &Path,
        final_order_status_path: &Path,
        final_user_fills_path: &Path,
    ) {
        let entries = [
            ("session.evidence.json", hash_file(evidence_path).unwrap()),
            ("operator-run.json", hash_file(config_path).unwrap()),
            ("session.journal.jsonl", hash_file(journal_path).unwrap()),
            (
                "independent-final-open-orders.json",
                hash_file(final_open_orders_path).unwrap(),
            ),
            (
                "independent-final-order-status.json",
                hash_file(final_order_status_path).unwrap(),
            ),
            (
                "independent-final-user-fills.json",
                hash_file(final_user_fills_path).unwrap(),
            ),
            ("hypercarry-testnet-operator", "d".repeat(64)),
            ("hypercarry-foundry-testnet-signer", "e".repeat(64)),
        ];
        let manifest = entries
            .into_iter()
            .fold(String::new(), |mut output, (name, digest)| {
                writeln!(output, "{digest}  {name}").unwrap();
                output
            });
        write_private(manifest_path, manifest.as_bytes());
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
