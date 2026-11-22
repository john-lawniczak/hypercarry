//! Strict, secret-free operational event and route contracts.
use anyhow::{Result, ensure};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt::Write as _, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    EntryFilled,
    ProtectionVerified,
    PositionClosed,
    FundingSummary,
    MarginWarning,
    StateUnknown,
    HeartbeatLost,
    ProtectionMissing,
    StopResidual,
    AgentRevoked,
    RecoveryFailed,
}
impl Kind {
    pub const fn severity(self) -> Severity {
        match self {
            Self::EntryFilled
            | Self::ProtectionVerified
            | Self::PositionClosed
            | Self::FundingSummary => Severity::Info,
            Self::MarginWarning => Severity::Warning,
            _ => Severity::Critical,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationEvent {
    pub schema_version: u32,
    pub id: String,
    pub network: String,
    pub trade_id: String,
    pub coin: String,
    pub kind: Kind,
    pub observed_at_ms: u64,
    #[serde(with = "optional_decimal")]
    pub remaining: Option<Decimal>,
}
impl NotificationEvent {
    /// # Errors
    /// Rejects malformed identities, scope, timestamps and negative exposure.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1
                && self.id.len() == 64
                && self.id.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid event identity"
        );
        ensure!(
            matches!(self.network.as_str(), "mainnet" | "testnet"),
            "invalid event network"
        );
        ensure!(
            label(&self.trade_id, 64) && label(&self.coin, 32),
            "invalid event labels"
        );
        ensure!(
            self.observed_at_ms > 0 && self.remaining.is_none_or(|q| q >= Decimal::ZERO),
            "invalid event time/exposure"
        );
        Ok(())
    }
    /// No arbitrary message body, addresses, signed requests or credentials.
    pub fn message(&self) -> String {
        format!(
            "Hypercarry {} {:?}: {:?}; trade={}; market={}; remaining={}; observed_ms={}; event={}",
            self.network,
            self.kind.severity(),
            self.kind,
            self.trade_id,
            self.coin,
            self.remaining
                .map_or_else(|| "unknown".into(), |q| q.to_string()),
            self.observed_at_ms,
            self.id.chars().take(12).collect::<String>()
        )
    }
}
pub fn label(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}
/// # Errors
/// Returns serialization errors; canonical typed values should be used for identities.
pub fn digest(value: &impl Serialize) -> Result<String> {
    Ok(Sha256::digest(serde_json::to_vec(value)?).iter().fold(
        String::with_capacity(64),
        |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        },
    ))
}
pub(crate) fn bytes_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Provider {
    Console,
    Resend {
        from: String,
        to: String,
        api_key_file: PathBuf,
    },
    Twilio {
        account_sid: String,
        from: String,
        to: String,
        auth_token_file: PathBuf,
    },
}
impl Provider {
    pub const fn idempotent(&self) -> bool {
        matches!(self, Self::Resend { .. })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub id: String,
    pub minimum_severity: Severity,
    #[serde(default)]
    pub kinds: Vec<Kind>,
    pub provider: Provider,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub path: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub state_dir: PathBuf,
    pub sources: Vec<Source>,
    pub routes: Vec<Route>,
    pub max_attempts: u32,
    pub retry_ms: u64,
    pub poll_ms: u64,
    pub delivery_timeout_ms: u64,
    pub sends_per_minute: u32,
    pub critical_reminder_ms: u64,
    pub max_reminders: u32,
}
impl Config {
    /// # Errors
    /// Rejects unbounded policies, duplicate routes/sources and malformed destinations.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1 && self.state_dir.is_absolute(),
            "invalid notification configuration"
        );
        ensure!(
            !self.routes.is_empty() && self.routes.len() <= 8 && self.sources.len() <= 8,
            "invalid route/source count"
        );
        ensure!(
            (1..=8).contains(&self.max_attempts)
                && (1000..=300_000).contains(&self.retry_ms)
                && (1000..=300_000).contains(&self.poll_ms),
            "invalid retry/poll bounds"
        );
        ensure!(
            (60_000..=86_400_000).contains(&self.delivery_timeout_ms)
                && (1..=60).contains(&self.sends_per_minute),
            "invalid delivery/rate bound"
        );
        ensure!(
            (60_000..=86_400_000).contains(&self.critical_reminder_ms) && self.max_reminders <= 3,
            "invalid reminder bounds"
        );
        let mut ids = std::collections::BTreeSet::new();
        for route in &self.routes {
            ensure!(
                label(&route.id, 32) && ids.insert(&route.id),
                "invalid/duplicate route id"
            );
            match &route.provider {
                Provider::Console => {}
                Provider::Resend {
                    from,
                    to,
                    api_key_file,
                } => {
                    for address in [from, to] {
                        ensure!(
                            address.len() <= 254
                                && address.contains('@')
                                && address.is_ascii()
                                && !address.bytes().any(|b| b.is_ascii_control()),
                            "invalid email address"
                        );
                    }
                    ensure!(api_key_file.is_absolute(), "secret path must be absolute");
                }
                Provider::Twilio {
                    account_sid,
                    from,
                    to,
                    auth_token_file,
                } => {
                    ensure!(
                        account_sid.len() == 34
                            && account_sid.starts_with("AC")
                            && account_sid[2..].bytes().all(|b| b.is_ascii_hexdigit()),
                        "invalid Twilio account id"
                    );
                    for number in [from, to] {
                        ensure!(
                            (9..=16).contains(&number.len())
                                && number.starts_with('+')
                                && number[1..].bytes().all(|b| b.is_ascii_digit()),
                            "SMS numbers must be E.164"
                        );
                    }
                    ensure!(
                        auth_token_file.is_absolute(),
                        "secret path must be absolute"
                    );
                }
            }
        }
        ids.clear();
        for source in &self.sources {
            ensure!(
                label(&source.id, 32) && ids.insert(&source.id) && source.path.is_absolute(),
                "invalid source"
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeliveryStatus {
    Queued,
    Sending,
    Accepted {
        provider_id: String,
    },
    Delivered {
        provider_id: String,
    },
    Failed {
        code: String,
    },
    Uncertain,
    /// Written to terminal; explicitly not a remote delivery receipt.
    Displayed,
    Superseded,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub id: String,
    pub route_id: String,
    pub event: NotificationEvent,
    pub generation: u32,
    pub status: DeliveryStatus,
    pub attempts: u32,
    pub first_attempt_ms: Option<u64>,
    pub next_at_ms: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendResult {
    Accepted(String),
    Displayed,
    Retry,
    Rejected,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receipt {
    Pending,
    Delivered,
    Failed,
    Unknown,
}
/// Implementations must use bounded I/O, never log provider response bodies/secrets,
/// and classify ambiguous writes as Unknown rather than a safe retry.
pub trait Sender {
    fn send(&mut self, route: &Route, delivery: &Delivery) -> SendResult;
    fn poll(&mut self, route: &Route, provider_id: &str) -> Receipt;
}

/// Decimal strings or null, including inside serde's internally tagged enums.
/// Deserializing through Option<String> also handles buffered unit/null values.
pub mod optional_decimal {
    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    /// # Errors
    /// Rejects non-string decimals and malformed values.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Decimal>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|s| s.parse::<Decimal>().map_err(serde::de::Error::custom))
            .transpose()
    }
    /// # Errors
    /// Propagates serializer failures.
    #[allow(clippy::ref_option)] // serde with-module signature.
    pub fn serialize<S: Serializer>(value: &Option<Decimal>, s: S) -> Result<S::Ok, S::Error> {
        value.map(|v| v.to_string()).serialize(s)
    }
}
