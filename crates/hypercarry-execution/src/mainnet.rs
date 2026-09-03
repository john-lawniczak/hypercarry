use crate::{ExecutionError, ExecutionNetwork, Signer, ValidatedOrder, validate_signer};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const REQUIRED_CLEAN_SESSIONS: usize = 3;
const MAX_EVIDENCE_BYTES: u64 = 1_048_576;

/// Exact interactive confirmation for manual mainnet authorization.
pub const MAINNET_CONFIRMATION: &str = "ENABLE HYPERCARRY MAINNET CANARY";

/// Proof that the host received the explicit mainnet command-line flag.
#[derive(Debug)]
pub struct ExplicitMainnetEnable(());

impl ExplicitMainnetEnable {
    /// Converts an explicit command-line flag into a typed gate input.
    ///
    /// # Errors
    ///
    /// Returns an error unless the flag was explicitly present and true.
    pub fn from_cli_flag(enabled: bool) -> Result<Self, ExecutionError> {
        if !enabled {
            return Err(gate_error(
                "explicit `--enable-mainnet` command-line flag is required",
            ));
        }
        Ok(Self(()))
    }
}

/// Proof of interactive confirmation for manual use.
#[derive(Debug)]
pub struct InteractiveMainnetConfirmation(());

impl InteractiveMainnetConfirmation {
    /// Validates the exact manual confirmation phrase.
    ///
    /// # Errors
    ///
    /// Returns an error for any other input.
    pub fn new(value: &str) -> Result<Self, ExecutionError> {
        if value != MAINNET_CONFIRMATION {
            return Err(gate_error(format!(
                "interactive confirmation must exactly match `{MAINNET_CONFIRMATION}`"
            )));
        }
        Ok(Self(()))
    }
}

/// Secret-free evidence from one completed credentialed testnet session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestnetSessionEvidence {
    /// Identifier of the credentialed testnet session.
    pub session_id: String,
    /// Source commit the session ran at.
    pub commit: String,
    /// Session start time, unix milliseconds.
    pub started_at_ms: i64,
    /// Session end time, unix milliseconds.
    pub ended_at_ms: i64,
    /// Independent reviewer who attested to the session.
    pub independent_reviewer: String,
    /// Independent final-state check result.
    pub final_state_check: String,
    /// Orders left open with no managing process.
    pub orphaned_orders: u32,
    /// Orders present that the harness did not manage.
    pub unmanaged_orders: u32,
    /// Submissions whose outcome was never resolved.
    pub unresolved_submissions: u32,
    /// Times a risk limit was bypassed.
    pub risk_limit_bypasses: u32,
    /// Detected secret leaks in session artifacts.
    pub secret_leaks: u32,
}

/// Explicit human release decision tied to reviewed evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanReleaseDecision {
    /// Whether the human approved the release.
    pub approved: bool,
    /// Identity of the approving human.
    pub approver: String,
    /// Decision time, unix milliseconds.
    pub decided_at_ms: i64,
    /// Digest of the evidence the decision was made over.
    pub evidence_digest: String,
}

/// Secret-free identities for the exact bundle reviewed for release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedReleaseBundle {
    /// Commit of the reviewed source tree.
    pub source_commit: String,
    /// Digest of the frozen `Cargo.lock`.
    pub cargo_lock_digest: String,
    /// Identifier of the reviewed mainnet transport artifact.
    pub mainnet_transport_artifact: String,
    /// Key identifier of the reviewed mainnet signer.
    pub mainnet_signer_key_id: String,
    /// Digest of the reviewed configuration.
    pub config_digest: String,
    /// Dependency audit reference.
    pub dependency_audit: String,
    /// Execution-focused security review reference.
    pub execution_security_review: String,
    /// Rollback exercise reference.
    pub rollback_test: String,
}

/// Durable evidence required before mainnet can authorize even a canary order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainnetReleaseEvidence {
    /// Version of the release-evidence contract.
    pub schema_version: u32,
    /// Independently reviewed clean testnet sessions.
    pub testnet_sessions: Vec<TestnetSessionEvidence>,
    /// The exact reviewed release bundle.
    pub reviewed_bundle: ReviewedReleaseBundle,
    /// The binding human release decision.
    pub release_decision: HumanReleaseDecision,
}

impl MainnetReleaseEvidence {
    /// Loads strict JSON evidence from disk.
    ///
    /// # Errors
    ///
    /// Returns an error for I/O, malformed JSON, unknown fields, or failed
    /// release invariants.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ExecutionError> {
        let path = path.as_ref();
        let length = fs::metadata(path)
            .map_err(|error| gate_error(format!("cannot inspect release evidence: {error}")))?
            .len();
        if length > MAX_EVIDENCE_BYTES {
            return Err(gate_error("release evidence exceeds 1 MiB"));
        }
        let bytes = fs::read(path)
            .map_err(|error| gate_error(format!("cannot read release evidence: {error}")))?;
        let evidence: Self = serde_json::from_slice(&bytes)
            .map_err(|_| gate_error("release evidence is not valid strict JSON"))?;
        evidence.validate()?;
        Ok(evidence)
    }

    /// Validates the complete evidence record.
    ///
    /// # Errors
    ///
    /// Returns an error until all recorded sessions and review decisions pass.
    pub fn validate(&self) -> Result<(), ExecutionError> {
        if self.schema_version != 2 {
            return Err(gate_error("release evidence schema must be version 2"));
        }
        if self.testnet_sessions.len() < REQUIRED_CLEAN_SESSIONS {
            return Err(gate_error(format!(
                "at least {REQUIRED_CLEAN_SESSIONS} clean credentialed testnet sessions are required"
            )));
        }
        if self.testnet_sessions.len() > 1_000 {
            return Err(gate_error(
                "release evidence must not exceed 1000 testnet sessions",
            ));
        }
        let mut ids = BTreeSet::new();
        for session in &self.testnet_sessions {
            validate_audit_id("testnet session ID", &session.session_id)?;
            validate_commit(&session.commit)?;
            if !ids.insert(&session.session_id) {
                return Err(gate_error("testnet session IDs must be unique"));
            }
            if session.started_at_ms < 0 || session.ended_at_ms <= session.started_at_ms {
                return Err(gate_error("testnet session interval is invalid"));
            }
            validate_audit_id(
                "independent session reviewer",
                &session.independent_reviewer,
            )?;
            validate_audit_id("independent final-state check", &session.final_state_check)?;
            if session.orphaned_orders != 0
                || session.unmanaged_orders != 0
                || session.unresolved_submissions != 0
                || session.risk_limit_bypasses != 0
                || session.secret_leaks != 0
            {
                return Err(gate_error(format!(
                    "testnet session {} is not clean",
                    session.session_id
                )));
            }
        }
        validate_exact_commit(&self.reviewed_bundle.source_commit)?;
        validate_digest(&self.reviewed_bundle.cargo_lock_digest)?;
        validate_audit_id(
            "mainnet transport artifact",
            &self.reviewed_bundle.mainnet_transport_artifact,
        )?;
        validate_audit_id(
            "mainnet signer key ID",
            &self.reviewed_bundle.mainnet_signer_key_id,
        )?;
        validate_digest(&self.reviewed_bundle.config_digest)?;
        validate_audit_id("dependency audit", &self.reviewed_bundle.dependency_audit)?;
        validate_audit_id(
            "execution security review",
            &self.reviewed_bundle.execution_security_review,
        )?;
        validate_audit_id("rollback test", &self.reviewed_bundle.rollback_test)?;
        if !self.release_decision.approved {
            return Err(gate_error("human release decision is not approved"));
        }
        validate_audit_id("release approver", &self.release_decision.approver)?;
        validate_digest(&self.release_decision.evidence_digest)?;
        if self.release_decision.evidence_digest != self.evidence_digest()? {
            return Err(gate_error(
                "human release decision digest does not match the evidence",
            ));
        }
        if self.release_decision.decided_at_ms
            <= self
                .testnet_sessions
                .iter()
                .map(|session| session.ended_at_ms)
                .max()
                .unwrap_or_default()
        {
            return Err(gate_error(
                "human release decision must follow all testnet sessions",
            ));
        }
        Ok(())
    }

    /// Computes the canonical SHA-256 digest reviewed by the human decision.
    ///
    /// # Errors
    ///
    /// Returns an error if the evidence payload cannot be serialized.
    pub fn evidence_digest(&self) -> Result<String, ExecutionError> {
        #[derive(Serialize)]
        struct DigestPayload<'a> {
            schema_version: u32,
            testnet_sessions: &'a [TestnetSessionEvidence],
            reviewed_bundle: &'a ReviewedReleaseBundle,
        }

        let encoded = serde_json::to_vec(&DigestPayload {
            schema_version: self.schema_version,
            testnet_sessions: &self.testnet_sessions,
            reviewed_bundle: &self.reviewed_bundle,
        })
        .map_err(|_| gate_error("cannot serialize release evidence digest payload"))?;
        let digest = Sha256::digest(encoded);
        let mut hexadecimal = String::with_capacity(64);
        for byte in digest {
            write!(&mut hexadecimal, "{byte:02x}")
                .map_err(|_| gate_error("cannot encode release evidence digest"))?;
        }
        Ok(hexadecimal)
    }
}

/// Reviewed single-market canary and runtime thresholds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainnetReleaseConfig {
    /// Commit this configuration was reviewed against.
    pub source_commit: String,
    /// Digest of the frozen `Cargo.lock`.
    pub cargo_lock_digest: String,
    /// The single allowlisted `venue:market` entry.
    pub allowed_market: String,
    /// Active canary notional ceiling.
    pub canary_notional_limit: Decimal,
    /// Reviewed maximum the canary limit may not exceed.
    pub reviewed_max_canary_notional: Decimal,
    /// Key identifier expected for the signer.
    pub testnet_key_id: String,
    /// Reviewed configuration revision identifier.
    pub config_revision: String,
    /// Configuration review reference.
    pub config_review: String,
    /// Maximum tolerated age of a health sample, milliseconds.
    pub max_health_age_ms: i64,
    /// Maximum tolerated REST latency, milliseconds.
    pub max_rest_latency_ms: u64,
    /// Maximum tolerated private-stream latency, milliseconds.
    pub max_private_latency_ms: u64,
    /// Time-to-live for a single order authorization, milliseconds.
    pub authorization_ttl_ms: u64,
}

impl MainnetReleaseConfig {
    /// Validates reviewed canary configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid market/key/revision identity, non-low
    /// canary, or non-positive health/latency bounds.
    pub fn validate(&self) -> Result<(), ExecutionError> {
        validate_exact_commit(&self.source_commit)?;
        validate_digest(&self.cargo_lock_digest)?;
        if self.allowed_market.trim().is_empty() || !self.allowed_market.contains(':') {
            return Err(gate_error(
                "mainnet requires exactly one `venue:market` allowlist entry",
            ));
        }
        if self.canary_notional_limit <= Decimal::ZERO
            || self.reviewed_max_canary_notional <= Decimal::ZERO
            || self.canary_notional_limit > self.reviewed_max_canary_notional
        {
            return Err(gate_error(
                "canary notional must be positive and within the reviewed maximum",
            ));
        }
        validate_audit_id("testnet key ID", &self.testnet_key_id)?;
        validate_audit_id("configuration revision", &self.config_revision)?;
        validate_audit_id("configuration review", &self.config_review)?;
        if self.max_health_age_ms <= 0
            || self.max_rest_latency_ms == 0
            || self.max_private_latency_ms == 0
            || self.authorization_ttl_ms == 0
            || self.authorization_ttl_ms > 60_000
        {
            return Err(gate_error(
                "health/latency thresholds must be positive and authorization TTL must be in 1..=60000ms",
            ));
        }
        Ok(())
    }

    /// Computes the canonical digest of every reviewed static gate value.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be serialized.
    pub fn review_digest(&self) -> Result<String, ExecutionError> {
        #[derive(Serialize)]
        struct ReviewPayload<'a> {
            source_commit: &'a str,
            cargo_lock_digest: &'a str,
            allowed_market: &'a str,
            canary_notional_limit: String,
            reviewed_max_canary_notional: String,
            testnet_key_id: &'a str,
            config_revision: &'a str,
            config_review: &'a str,
            max_health_age_ms: i64,
            max_rest_latency_ms: u64,
            max_private_latency_ms: u64,
            authorization_ttl_ms: u64,
        }

        let encoded = serde_json::to_vec(&ReviewPayload {
            source_commit: &self.source_commit,
            cargo_lock_digest: &self.cargo_lock_digest,
            allowed_market: &self.allowed_market,
            canary_notional_limit: self.canary_notional_limit.normalize().to_string(),
            reviewed_max_canary_notional: self.reviewed_max_canary_notional.normalize().to_string(),
            testnet_key_id: &self.testnet_key_id,
            config_revision: &self.config_revision,
            config_review: &self.config_review,
            max_health_age_ms: self.max_health_age_ms,
            max_rest_latency_ms: self.max_rest_latency_ms,
            max_private_latency_ms: self.max_private_latency_ms,
            authorization_ttl_ms: self.authorization_ttl_ms,
        })
        .map_err(|_| gate_error("cannot serialize reviewed mainnet configuration"))?;
        Ok(hex_digest(&encoded))
    }
}

/// Startup and continuous operational state sampled before every authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// The checked subsystem is ready.
    Ready,
    /// The checked subsystem is not ready; authorization must fail closed.
    NotReady,
}

/// Startup and continuous operational state sampled before every authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalHealth {
    /// Time the snapshot was observed, unix milliseconds.
    pub observed_at_ms: i64,
    /// Startup reconciliation readiness.
    pub startup_reconciliation: Readiness,
    /// Continuous reconciliation readiness.
    pub continuous_reconciliation: Readiness,
    /// Orders present that the operator does not manage.
    pub unmanaged_orders: u32,
    /// Submissions whose outcome is unresolved.
    pub unresolved_submissions: u32,
    /// Most recent REST latency, milliseconds.
    pub rest_latency_ms: u64,
    /// Most recent private-stream latency, milliseconds.
    pub private_latency_ms: u64,
    /// Private-stream readiness.
    pub private_stream: Readiness,
    /// Alerting subsystem readiness.
    pub alerts: Readiness,
    /// Audit journal readiness.
    pub audit_journal: Readiness,
    /// Rollback capability readiness.
    pub rollback: Readiness,
}

/// Source of fresh health, latency, reconciliation, alert, and audit metrics.
pub trait OperationalHealthSource {
    /// Returns a current operational snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when health cannot be established; authorization fails
    /// closed.
    fn health(&self) -> Result<OperationalHealth, ExecutionError>;
}

/// Process-independent dead-man observation.
pub trait DeadManSwitch {
    /// Reports whether a heartbeat is present and fresh at `now_ms`.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable/malformed state or invalid time.
    fn is_armed(&self, now_ms: i64) -> Result<bool, ExecutionError>;
}

/// Filesystem heartbeat dead-man switch shared across processes.
pub struct FileDeadManSwitch {
    heartbeat_path: PathBuf,
    timeout_ms: u64,
}

impl FileDeadManSwitch {
    /// Creates a heartbeat monitor with a bounded positive timeout.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero timeout.
    pub fn new(
        heartbeat_path: impl Into<PathBuf>,
        timeout_ms: u64,
    ) -> Result<Self, ExecutionError> {
        if timeout_ms == 0 {
            return Err(gate_error("dead-man timeout must be positive"));
        }
        Ok(Self {
            heartbeat_path: heartbeat_path.into(),
            timeout_ms,
        })
    }
}

impl DeadManSwitch for FileDeadManSwitch {
    fn is_armed(&self, now_ms: i64) -> Result<bool, ExecutionError> {
        if now_ms < 0 {
            return Err(gate_error("dead-man check time precedes epoch"));
        }
        let metadata = match fs::metadata(&self.heartbeat_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(gate_error(format!(
                    "cannot read dead-man heartbeat: {error}"
                )));
            }
        };
        let modified_ms = metadata
            .modified()
            .map_err(|_| gate_error("dead-man heartbeat modification time is unavailable"))?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| gate_error("dead-man heartbeat predates epoch"))?
            .as_millis();
        let modified_ms = i64::try_from(modified_ms)
            .map_err(|_| gate_error("dead-man heartbeat time exceeds i64"))?;
        let age_ms = now_ms
            .checked_sub(modified_ms)
            .ok_or_else(|| gate_error("dead-man heartbeat is in the future"))?;
        Ok(age_ms >= 0 && u64::try_from(age_ms).is_ok_and(|age| age <= self.timeout_ms))
    }
}

/// Unforgeable result required by any future mainnet adapter.
#[derive(Debug)]
pub struct MainnetAuthorization {
    order: ValidatedOrder,
    market: String,
    max_notional: Decimal,
    config_revision: String,
    evidence_digest: String,
    authorized_at_ms: i64,
    expires_at_ms: i64,
}

impl MainnetAuthorization {
    /// Allowlisted market this authorization is bound to.
    pub fn market(&self) -> &str {
        &self.market
    }

    /// Maximum notional the authorized order may carry.
    pub const fn max_notional(&self) -> Decimal {
        self.max_notional
    }

    /// Reviewed configuration revision this authorization was issued under.
    pub fn config_revision(&self) -> &str {
        &self.config_revision
    }

    /// Time the authorization was issued, unix milliseconds.
    pub const fn authorized_at_ms(&self) -> i64 {
        self.authorized_at_ms
    }

    /// Time the authorization expires, unix milliseconds.
    pub const fn expires_at_ms(&self) -> i64 {
        self.expires_at_ms
    }

    /// Digest of the evidence the authorization was issued against.
    pub fn evidence_digest(&self) -> &str {
        &self.evidence_digest
    }

    /// Consumes this single-use capability and returns its exact reviewed order.
    ///
    /// # Errors
    ///
    /// Returns an error once the short authorization lifetime has elapsed.
    pub fn into_order(self, now_ms: i64) -> Result<ValidatedOrder, ExecutionError> {
        if now_ms < self.authorized_at_ms || now_ms > self.expires_at_ms {
            return Err(gate_error("mainnet authorization is not currently valid"));
        }
        Ok(self.order)
    }
}

/// Fail-closed mainnet release gate. This module contains no mainnet transport.
pub struct MainnetReleaseGate<H, D> {
    config: MainnetReleaseConfig,
    evidence: MainnetReleaseEvidence,
    health: H,
    dead_man: D,
}

impl<H, D> MainnetReleaseGate<H, D>
where
    H: OperationalHealthSource,
    D: DeadManSwitch,
{
    /// Creates the gate after static configuration/evidence validation.
    ///
    /// # Errors
    ///
    /// Returns an error while evidence or canary configuration is incomplete.
    pub fn new(
        config: MainnetReleaseConfig,
        evidence: MainnetReleaseEvidence,
        health: H,
        dead_man: D,
    ) -> Result<Self, ExecutionError> {
        config.validate()?;
        evidence.validate()?;
        validate_bundle_binding(&config, &evidence)?;
        Ok(Self {
            config,
            evidence,
            health,
            dead_man,
        })
    }

    /// Rechecks credentials, order canary, continuous health, reconciliation,
    /// alert/audit readiness, rollback, and dead-man state.
    ///
    /// The explicit enable/confirmation values are consumed for each manual
    /// authorization and cannot be reused by reference.
    ///
    /// # Errors
    ///
    /// Returns an error for any missing, stale, mismatched, or over-limit gate.
    pub fn authorize<S: Signer>(
        &self,
        order: &ValidatedOrder,
        now_ms: i64,
        _enable: ExplicitMainnetEnable,
        _confirmation: InteractiveMainnetConfirmation,
        signer: &S,
    ) -> Result<MainnetAuthorization, ExecutionError> {
        order.validate()?;
        self.config.validate()?;
        self.evidence.validate()?;
        validate_bundle_binding(&self.config, &self.evidence)?;
        validate_signer(signer, ExecutionNetwork::Mainnet)?;
        if signer.key_id() == self.config.testnet_key_id {
            return Err(gate_error(
                "mainnet and testnet must use separate credential aliases",
            ));
        }
        if signer.key_id() != self.evidence.reviewed_bundle.mainnet_signer_key_id {
            return Err(gate_error(
                "mainnet signer does not match the reviewed release bundle",
            ));
        }
        let market = format!("{}:{}", order.venue, order.market);
        if market != self.config.allowed_market {
            return Err(gate_error(format!(
                "mainnet canary market {market} is not the single reviewed market"
            )));
        }
        let notional = order
            .quantity
            .checked_mul(order.limit_price)
            .ok_or_else(|| gate_error("mainnet canary notional overflow"))?
            .normalize();
        if notional > self.config.canary_notional_limit {
            return Err(gate_error(format!(
                "order notional {notional} exceeds canary limit {}",
                self.config.canary_notional_limit
            )));
        }
        let health = self.health.health()?;
        validate_health(&self.config, &health, now_ms)?;
        if !self.dead_man.is_armed(now_ms)? {
            return Err(gate_error("dead-man heartbeat is missing or stale"));
        }
        let ttl_ms = i64::try_from(self.config.authorization_ttl_ms)
            .map_err(|_| gate_error("mainnet authorization TTL exceeds i64"))?;
        let expires_at_ms = now_ms
            .checked_add(ttl_ms)
            .ok_or_else(|| gate_error("mainnet authorization expiry overflow"))?;
        Ok(MainnetAuthorization {
            order: order.clone(),
            market,
            max_notional: self.config.canary_notional_limit,
            config_revision: self.config.config_revision.clone(),
            evidence_digest: self.evidence.evidence_digest()?,
            authorized_at_ms: now_ms,
            expires_at_ms,
        })
    }
}

fn validate_bundle_binding(
    config: &MainnetReleaseConfig,
    evidence: &MainnetReleaseEvidence,
) -> Result<(), ExecutionError> {
    if evidence.reviewed_bundle.source_commit != config.source_commit
        || evidence.reviewed_bundle.cargo_lock_digest != config.cargo_lock_digest
        || evidence.reviewed_bundle.config_digest != config.review_digest()?
    {
        return Err(gate_error(
            "runtime code, lockfile, or configuration differs from the reviewed release bundle",
        ));
    }
    Ok(())
}

fn validate_health(
    config: &MainnetReleaseConfig,
    health: &OperationalHealth,
    now_ms: i64,
) -> Result<(), ExecutionError> {
    if now_ms < 0 || health.observed_at_ms < 0 {
        return Err(gate_error("health timestamps must not precede epoch"));
    }
    let age = now_ms
        .checked_sub(health.observed_at_ms)
        .ok_or_else(|| gate_error("health observation is in the future"))?;
    if age > config.max_health_age_ms {
        return Err(gate_error("operational health snapshot is stale"));
    }
    if health.startup_reconciliation != Readiness::Ready
        || health.continuous_reconciliation != Readiness::Ready
    {
        return Err(gate_error(
            "startup and continuous reconciliation are required",
        ));
    }
    if health.unmanaged_orders != 0 || health.unresolved_submissions != 0 {
        return Err(gate_error(
            "unmanaged orders or unresolved submissions remain",
        ));
    }
    if health.rest_latency_ms > config.max_rest_latency_ms
        || health.private_latency_ms > config.max_private_latency_ms
    {
        return Err(gate_error(
            "REST or private-stream latency exceeds threshold",
        ));
    }
    if health.private_stream != Readiness::Ready
        || health.alerts != Readiness::Ready
        || health.audit_journal != Readiness::Ready
        || health.rollback != Readiness::Ready
    {
        return Err(gate_error(
            "private stream, alerts, audit log, and rollback readiness are required",
        ));
    }
    Ok(())
}

fn validate_audit_id(field: &str, value: &str) -> Result<(), ExecutionError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
    {
        return Err(gate_error(format!(
            "{field} must be a bounded secret-free audit identifier"
        )));
    }
    Ok(())
}

fn validate_commit(value: &str) -> Result<(), ExecutionError> {
    if !(7..=40).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(gate_error("testnet evidence commit is invalid"));
    }
    Ok(())
}

fn validate_exact_commit(value: &str) -> Result<(), ExecutionError> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(gate_error(
            "reviewed source commit must be a full lowercase SHA-1 identifier",
        ));
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), ExecutionError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(gate_error(
            "release evidence digest must be 32-byte hexadecimal",
        ));
    }
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hexadecimal = String::with_capacity(64);
    for byte in digest {
        write!(&mut hexadecimal, "{byte:02x}")
            .expect("writing hexadecimal into a String cannot fail");
    }
    hexadecimal
}

fn gate_error(message: impl Into<String>) -> ExecutionError {
    ExecutionError::ReleaseGate(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MarketMetadata, OrderIntent, Side};
    use std::{cell::Cell, convert::Infallible};

    struct Health(OperationalHealth);

    impl OperationalHealthSource for Health {
        fn health(&self) -> Result<OperationalHealth, ExecutionError> {
            Ok(self.0.clone())
        }
    }

    struct DeadMan(bool);

    impl DeadManSwitch for DeadMan {
        fn is_armed(&self, _now_ms: i64) -> Result<bool, ExecutionError> {
            Ok(self.0)
        }
    }

    struct Provider {
        network: ExecutionNetwork,
        key_id: &'static str,
        calls: Cell<usize>,
    }

    impl Signer for Provider {
        type Error = Infallible;

        fn network(&self) -> ExecutionNetwork {
            self.network
        }

        fn key_id(&self) -> &'static str {
            self.key_id
        }

        fn sign(&self, _payload: &[u8]) -> Result<Vec<u8>, Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(Vec::new())
        }
    }

    #[test]
    fn all_exact_gates_authorize_single_market_canary_without_signing() {
        let gate = gate(healthy(), DeadMan(true));
        let signer = mainnet_signer("provider/mainnet-key");
        let expected_order = order("1", "100");
        let authorization = gate
            .authorize(
                &expected_order,
                10_100,
                ExplicitMainnetEnable::from_cli_flag(true).unwrap(),
                InteractiveMainnetConfirmation::new(MAINNET_CONFIRMATION).unwrap(),
                &signer,
            )
            .unwrap();

        assert_eq!(authorization.market(), "hyperliquid:BTC");
        assert_eq!(authorization.max_notional(), dec("100"));
        assert_eq!(authorization.into_order(10_101).unwrap(), expected_order);
        assert_eq!(signer.calls.get(), 0);
    }

    #[test]
    fn incomplete_evidence_and_unclean_session_keep_release_closed() {
        let mut incomplete_evidence = evidence();
        incomplete_evidence.testnet_sessions.pop();
        assert!(incomplete_evidence.validate().is_err());
        let mut unclean_evidence = evidence();
        unclean_evidence.testnet_sessions[1].unmanaged_orders = 1;
        assert!(unclean_evidence.validate().is_err());

        let mut tampered_evidence = evidence();
        tampered_evidence.reviewed_bundle.execution_security_review =
            "review/security-2".to_owned();
        assert!(tampered_evidence.validate().is_err());
    }

    #[test]
    fn reviewed_bundle_mismatch_and_expired_authorization_fail_closed() {
        let mut changed_config = config();
        changed_config.max_rest_latency_ms = 499;
        assert!(
            MainnetReleaseGate::new(changed_config, evidence(), Health(healthy()), DeadMan(true),)
                .is_err()
        );

        let signer = mainnet_signer("provider/mainnet-key");
        let authorization = authorize(&gate(healthy(), DeadMan(true)), &signer, "1").unwrap();
        assert!(authorization.into_order(15_101).is_err());
    }

    #[test]
    fn separate_credentials_and_explicit_manual_gates_are_required() {
        assert!(ExplicitMainnetEnable::from_cli_flag(false).is_err());
        assert!(InteractiveMainnetConfirmation::new("yes").is_err());
        let gate = gate(healthy(), DeadMan(true));
        let same_key = mainnet_signer("provider/testnet-key");
        assert!(
            gate.authorize(
                &order("1", "100"),
                10_100,
                ExplicitMainnetEnable::from_cli_flag(true).unwrap(),
                InteractiveMainnetConfirmation::new(MAINNET_CONFIRMATION).unwrap(),
                &same_key,
            )
            .is_err()
        );
        let unreviewed_key = mainnet_signer("provider/unreviewed-mainnet-key");
        assert!(
            gate.authorize(
                &order("1", "100"),
                10_100,
                ExplicitMainnetEnable::from_cli_flag(true).unwrap(),
                InteractiveMainnetConfirmation::new(MAINNET_CONFIRMATION).unwrap(),
                &unreviewed_key,
            )
            .is_err()
        );
    }

    #[test]
    fn canary_health_latency_and_dead_man_fail_closed_beyond_boundaries() {
        let signer = mainnet_signer("provider/mainnet-key");
        assert!(authorize(&gate(healthy(), DeadMan(true)), &signer, "1.01").is_err());

        let mut stale = healthy();
        stale.observed_at_ms = 5_000;
        assert!(authorize(&gate(stale, DeadMan(true)), &signer, "1").is_err());

        let mut slow = healthy();
        slow.rest_latency_ms = 501;
        assert!(authorize(&gate(slow, DeadMan(true)), &signer, "1").is_err());
        assert!(authorize(&gate(healthy(), DeadMan(false)), &signer, "1").is_err());

        let overflow = order(&Decimal::MAX.to_string(), "2");
        let gate = gate(healthy(), DeadMan(true));
        assert!(
            gate.authorize(
                &overflow,
                10_100,
                ExplicitMainnetEnable::from_cli_flag(true).unwrap(),
                InteractiveMainnetConfirmation::new(MAINNET_CONFIRMATION).unwrap(),
                &signer,
            )
            .is_err()
        );
    }

    #[test]
    fn filesystem_dead_man_requires_a_present_fresh_heartbeat() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("heartbeat");
        let dead_man = FileDeadManSwitch::new(&path, 10_000).unwrap();
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        assert!(!dead_man.is_armed(now_ms).unwrap());
        std::fs::write(path, b"heartbeat\n").unwrap();
        let now_ms = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        assert!(dead_man.is_armed(now_ms).unwrap());
    }

    fn authorize(
        gate: &MainnetReleaseGate<Health, DeadMan>,
        signer: &Provider,
        quantity: &str,
    ) -> Result<MainnetAuthorization, ExecutionError> {
        gate.authorize(
            &order(quantity, "100"),
            10_100,
            ExplicitMainnetEnable::from_cli_flag(true).unwrap(),
            InteractiveMainnetConfirmation::new(MAINNET_CONFIRMATION).unwrap(),
            signer,
        )
    }

    fn gate(health: OperationalHealth, dead_man: DeadMan) -> MainnetReleaseGate<Health, DeadMan> {
        MainnetReleaseGate::new(config(), evidence(), Health(health), dead_man).unwrap()
    }

    fn config() -> MainnetReleaseConfig {
        MainnetReleaseConfig {
            source_commit: "a".repeat(40),
            cargo_lock_digest: "11".repeat(32),
            allowed_market: "hyperliquid:BTC".to_owned(),
            canary_notional_limit: dec("100"),
            reviewed_max_canary_notional: dec("100"),
            testnet_key_id: "provider/testnet-key".to_owned(),
            config_revision: "mainnet-canary-v1".to_owned(),
            config_review: "review/config-1".to_owned(),
            max_health_age_ms: 100,
            max_rest_latency_ms: 500,
            max_private_latency_ms: 500,
            authorization_ttl_ms: 5_000,
        }
    }

    fn evidence() -> MainnetReleaseEvidence {
        let config = config();
        let mut evidence = MainnetReleaseEvidence {
            schema_version: 2,
            testnet_sessions: (0..3)
                .map(|index| TestnetSessionEvidence {
                    session_id: format!("session-{index}"),
                    commit: "9da7b4c".to_owned(),
                    started_at_ms: i64::from(index) * 1_000,
                    ended_at_ms: i64::from(index) * 1_000 + 900,
                    independent_reviewer: format!("reviewer-{index}"),
                    final_state_check: format!("check/final-state-{index}"),
                    orphaned_orders: 0,
                    unmanaged_orders: 0,
                    unresolved_submissions: 0,
                    risk_limit_bypasses: 0,
                    secret_leaks: 0,
                })
                .collect(),
            reviewed_bundle: ReviewedReleaseBundle {
                source_commit: config.source_commit.clone(),
                cargo_lock_digest: config.cargo_lock_digest.clone(),
                mainnet_transport_artifact: "review/transport-1".to_owned(),
                mainnet_signer_key_id: "provider/mainnet-key".to_owned(),
                config_digest: config.review_digest().unwrap(),
                dependency_audit: "audit/dependencies-1".to_owned(),
                execution_security_review: "review/security-1".to_owned(),
                rollback_test: "test/rollback-1".to_owned(),
            },
            release_decision: HumanReleaseDecision {
                approved: true,
                approver: "operator-1".to_owned(),
                decided_at_ms: 5_000,
                evidence_digest: String::new(),
            },
        };
        evidence.release_decision.evidence_digest = evidence.evidence_digest().unwrap();
        evidence
    }

    fn healthy() -> OperationalHealth {
        OperationalHealth {
            observed_at_ms: 10_000,
            startup_reconciliation: Readiness::Ready,
            continuous_reconciliation: Readiness::Ready,
            unmanaged_orders: 0,
            unresolved_submissions: 0,
            rest_latency_ms: 500,
            private_latency_ms: 500,
            private_stream: Readiness::Ready,
            alerts: Readiness::Ready,
            audit_journal: Readiness::Ready,
            rollback: Readiness::Ready,
        }
    }

    fn mainnet_signer(key_id: &'static str) -> Provider {
        Provider {
            network: ExecutionNetwork::Mainnet,
            key_id,
            calls: Cell::new(0),
        }
    }

    fn order(quantity: &str, price: &str) -> ValidatedOrder {
        let intent = OrderIntent::limit(
            "mainnet-canary-1",
            "hyperliquid",
            "BTC",
            Side::Buy,
            dec(quantity),
            dec(price),
            1_000,
        )
        .unwrap();
        let metadata =
            MarketMetadata::new("hyperliquid", "BTC", dec("0.1"), dec("0.01"), dec("0.01"))
                .unwrap();
        ValidatedOrder::resolve(&intent, &metadata).unwrap()
    }

    fn dec(value: &str) -> Decimal {
        value.parse().unwrap()
    }
}
