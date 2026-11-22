//! Single-writer, atomic local outbox. Journal cursor and enqueued events commit together.
use crate::{
    Config, Delivery, DeliveryStatus, NotificationEvent, Receipt, SendResult, Sender, Severity,
    digest, model::bytes_digest,
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};

const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DELIVERIES: usize = 10_000;

/// # Errors
/// Rejects non-directories, symlinks and group/other-accessible local state roots.
pub fn private_dir(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "private directory must be absolute");
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink(),
        "private directory must not be a symlink"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            m.permissions().mode().trailing_zeros() >= 6,
            "directory must have mode 0700"
        );
    }
    Ok(())
}
/// # Errors
/// Rejects public, nonregular, symlinked, hardlinked or oversized inputs.
pub fn read_private(path: &Path) -> Result<Vec<u8>> {
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags, open};
        File::from(open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )?)
    };
    #[cfg(not(unix))]
    let file = {
        ensure!(
            !fs::symlink_metadata(path)?.file_type().is_symlink(),
            "symlinked private file"
        );
        File::open(path)?
    };
    validate_private_file(&file.metadata()?)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "private input exceeds size bound"
    );
    Ok(bytes)
}

fn validate_private_file(m: &fs::Metadata) -> Result<()> {
    ensure!(
        m.is_file() && !m.file_type().is_symlink() && m.len() <= MAX_BYTES,
        "invalid private file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(
            m.permissions().mode().trailing_zeros() >= 6 && m.nlink() == 1,
            "file must be private and not hardlinked"
        );
    }
    Ok(())
}
/// Atomically replace a private file with exact bytes, without serializing.
/// Shared by [`write_private`] and other callers that already hold a complete
/// encoded document, such as an appended JSON-lines journal.
/// # Errors
/// Returns filesystem errors; syncs contents and parent before success.
pub fn write_private_bytes(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().context("state file has no parent")?;
    private_dir(parent)?;
    if path.symlink_metadata().is_ok() {
        read_private(path)?;
    }
    ensure!(data.len() as u64 <= MAX_BYTES, "state exceeds size bound");
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(data)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
/// # Errors
/// Returns filesystem/serialization errors; syncs contents and parent before success.
pub fn write_private<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_private_bytes(path, &serde_json::to_vec(value)?)
}
/// # Errors
/// Rejects unsafe paths or another writer. Hold the returned file for the whole session.
pub fn lock_private(path: &Path) -> Result<File> {
    private_dir(path.parent().context("lock has no parent")?)?;
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags, open};
        File::from(open(
            path,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )?)
    };
    #[cfg(not(unix))]
    let file = {
        if path.symlink_metadata().is_ok() {
            read_private(path)?;
        }
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?
    };
    validate_private_file(&file.metadata()?)?;
    file.try_lock_exclusive()
        .context("another local writer holds the lock")?;
    Ok(file)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    offset: usize,
    prefix_digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutboxState {
    schema_version: u32,
    config_digest: String,
    last_time_ms: u64,
    cursors: BTreeMap<String, Cursor>,
    seen: BTreeMap<String, String>,
    pub deliveries: BTreeMap<String, Delivery>,
    pub acknowledged: BTreeMap<String, u64>,
    pub resolved: BTreeMap<String, u64>,
    rate: Vec<(String, u64)>,
}
pub struct Outbox {
    config: Config,
    state: OutboxState,
    _lock: File,
}
impl Outbox {
    /// # Errors
    /// Rejects unsafe state, configuration drift, malformed persistence or concurrent writers.
    pub fn open(config: Config) -> Result<Self> {
        config.validate()?;
        private_dir(&config.state_dir)?;
        let lock = lock_private(&config.state_dir.join("outbox.lock"))?;
        let bound = digest(&config)?;
        let path = config.state_dir.join("outbox.json");
        let state = if path.symlink_metadata().is_ok() {
            serde_json::from_slice::<OutboxState>(&read_private(&path)?)?
        } else {
            OutboxState {
                schema_version: 1,
                config_digest: bound.clone(),
                last_time_ms: 0,
                cursors: BTreeMap::new(),
                seen: BTreeMap::new(),
                deliveries: BTreeMap::new(),
                acknowledged: BTreeMap::new(),
                resolved: BTreeMap::new(),
                rate: Vec::new(),
            }
        };
        ensure!(
            state.schema_version == 1 && state.config_digest == bound,
            "outbox belongs to another configuration; explicit migration required"
        );
        for source in &config.sources {
            // A missing/unreadable source must not prevent opening the durable
            // outbox to drain deliveries or acknowledge an existing incident.
            let p = source.path.canonicalize().ok().or_else(|| {
                Some(
                    source
                        .path
                        .parent()?
                        .canonicalize()
                        .ok()?
                        .join(source.path.file_name()?),
                )
            });
            ensure!(
                p.as_ref() != Some(&config.state_dir.canonicalize()?.join("outbox.json"))
                    && p.as_ref() != Some(&config.state_dir.canonicalize()?.join("outbox.lock")),
                "source aliases mutable outbox state"
            );
        }
        Ok(Self {
            config,
            state,
            _lock: lock,
        })
    }
    pub fn state(&self) -> &OutboxState {
        &self.state
    }
    fn commit(&mut self, next: OutboxState) -> Result<()> {
        write_private(&self.config.state_dir.join("outbox.json"), &next)?;
        self.state = next;
        Ok(())
    }
    fn checked_time(&self, now: u64) -> Result<()> {
        ensure!(
            now > 0 && now >= self.state.last_time_ms,
            "notification clock regressed"
        );
        Ok(())
    }
    fn enqueue_into(
        config: &Config,
        state: &mut OutboxState,
        event: &NotificationEvent,
        now: u64,
    ) -> Result<()> {
        event.validate()?;
        ensure!(event.observed_at_ms <= now, "future notification event");
        let fingerprint = digest(event)?;
        if let Some(old) = state.seen.get(&event.id) {
            ensure!(*old == fingerprint, "conflicting event duplicate");
            return Ok(());
        }
        ensure!(
            state.seen.len() < MAX_DELIVERIES,
            "event retention capacity reached; archive/migrate explicitly"
        );
        for d in state.deliveries.values() {
            if d.event.network == event.network
                && d.event.trade_id == event.trade_id
                && d.event.coin == event.coin
                && d.event.observed_at_ms <= event.observed_at_ms
                && (event.kind == crate::Kind::PositionClosed
                    || (event.kind == crate::Kind::ProtectionVerified
                        && matches!(
                            d.event.kind,
                            crate::Kind::ProtectionMissing | crate::Kind::StateUnknown
                        )))
            {
                state.resolved.insert(d.event.id.clone(), now);
            }
        }
        for route in &config.routes {
            if event.kind.severity() < route.minimum_severity
                || (!route.kinds.is_empty() && !route.kinds.contains(&event.kind))
            {
                continue;
            }
            Self::add_delivery(state, event, &route.id, 0, now)?;
        }
        state.seen.insert(event.id.clone(), fingerprint);
        Ok(())
    }
    fn add_delivery(
        state: &mut OutboxState,
        event: &NotificationEvent,
        route: &str,
        generation: u32,
        now: u64,
    ) -> Result<()> {
        let id = digest(&(&event.id, route, generation))?;
        if state.deliveries.contains_key(&id) {
            return Ok(());
        }
        ensure!(
            state.deliveries.len() < MAX_DELIVERIES,
            "outbox capacity reached; archive/migrate explicitly"
        );
        state
            .deliveries
            .entry(id.clone())
            .or_insert_with(|| Delivery {
                id,
                route_id: route.into(),
                event: event.clone(),
                generation,
                status: DeliveryStatus::Queued,
                attempts: 0,
                first_attempt_ms: None,
                next_at_ms: now,
            });
        Ok(())
    }
    /// # Errors
    /// Rejects invalid/conflicting events or failed durable writes.
    pub fn enqueue(&mut self, event: &NotificationEvent, now: u64) -> Result<()> {
        self.checked_time(now)?;
        let mut next = self.state.clone();
        next.last_time_ms = now;
        Self::enqueue_into(&self.config, &mut next, event, now)?;
        self.commit(next)
    }
    /// Import complete JSONL records containing `notification` at the root or under `event`.
    /// Incomplete final lines wait for the next pass; prefix changes fail closed.
    /// # Errors
    /// Rejects source truncation/mutation, malformed records and conflicting events.
    pub fn ingest(&mut self, now: u64) -> Result<()> {
        let failed = self.ingest_sources(now)?;
        ensure!(
            failed.is_empty(),
            "notification sources unavailable: {}",
            failed.join(", ")
        );
        Ok(())
    }
    /// Import each source atomically. A failing source keeps its old cursor and
    /// does not block healthy sources. Returns source IDs, never source contents.
    /// # Errors
    /// Returns clock or persistence errors. Source errors are returned as IDs.
    pub fn ingest_sources(&mut self, now: u64) -> Result<Vec<String>> {
        self.checked_time(now)?;
        let mut failed = Vec::new();
        for source in self.config.sources.clone() {
            let mut next = self.state.clone();
            next.last_time_ms = now;
            let imported = (|| -> Result<()> {
                let bytes = read_private(&source.path)?;
                let offset = if let Some(c) = next.cursors.get(&source.id) {
                    ensure!(
                        bytes.len() >= c.offset
                            && bytes_digest(&bytes[..c.offset]) == c.prefix_digest,
                        "journal prefix changed; refusing replay/resend"
                    );
                    c.offset
                } else {
                    0
                };
                let mut consumed = offset;
                for line in bytes[offset..].split_inclusive(|b| *b == b'\n') {
                    if !line.ends_with(b"\n") {
                        break;
                    }
                    ensure!(line.len() <= 1_048_576, "journal record exceeds bound");
                    let value: serde_json::Value =
                        serde_json::from_slice(line).context("invalid source journal record")?;
                    if let Some(value) = value
                        .get("notification")
                        .or_else(|| value.get("event").and_then(|v| v.get("notification")))
                        .filter(|v| !v.is_null())
                    {
                        let event: NotificationEvent = serde_json::from_value(value.clone())?;
                        Self::enqueue_into(&self.config, &mut next, &event, now)?;
                    }
                    consumed += line.len();
                }
                next.cursors.insert(
                    source.id.clone(),
                    Cursor {
                        offset: consumed,
                        prefix_digest: bytes_digest(&bytes[..consumed]),
                    },
                );
                Ok(())
            })();
            if imported.is_err() {
                failed.push(source.id);
                continue;
            }
            self.commit(next)?;
        }
        Ok(failed)
    }
    /// Drain existing deliveries even when a source fails; surface failed source
    /// IDs so the worker can report degraded ingestion and continue polling.
    /// # Errors
    /// Returns outbox persistence, clock or delivery policy errors.
    pub fn run_pass(
        &mut self,
        sender: &mut impl Sender,
        mut clock: impl FnMut() -> Result<u64>,
    ) -> Result<Vec<String>> {
        let failed = self.ingest_sources(clock()?)?;
        self.tick_with_clock(sender, clock)?;
        Ok(failed)
    }
    /// Human acknowledgement is distinct from provider delivery confirmation.
    /// # Errors
    /// Rejects unknown events, regressed time and failed durable writes.
    pub fn acknowledge(&mut self, event_id: &str, now: u64) -> Result<()> {
        self.checked_time(now)?;
        ensure!(self.state.seen.contains_key(event_id), "unknown event");
        let mut next = self.state.clone();
        next.acknowledged.insert(event_id.into(), now);
        next.last_time_ms = now;
        self.commit(next)
    }
    /// Bounded worker pass. State is persisted before every external send.
    /// # Errors
    /// Returns persistence or policy errors. Sender I/O failures become explicit states.
    pub fn tick(&mut self, sender: &mut impl Sender, now: u64) -> Result<()> {
        self.tick_with_clock(sender, || Ok(now))
    }
    /// Worker pass with time refreshed around provider I/O. The fixed-time
    /// `tick` helper is for deterministic callers; production uses this clock.
    /// # Errors
    /// Returns persistence, clock or policy errors.
    #[allow(clippy::too_many_lines)]
    pub fn tick_with_clock(
        &mut self,
        sender: &mut impl Sender,
        mut clock: impl FnMut() -> Result<u64>,
    ) -> Result<()> {
        let now = clock()?;
        self.checked_time(now)?;
        let mut next = self.state.clone();
        next.last_time_ms = now;
        next.rate.retain(|(_, at)| now.saturating_sub(*at) < 60_000);
        // A crash after reservation may have sent a message. Only the provider's
        // bounded idempotency contract permits an automatic resend.
        for d in next.deliveries.values_mut() {
            if d.status == DeliveryStatus::Sending {
                let route = self
                    .config
                    .routes
                    .iter()
                    .find(|r| r.id == d.route_id)
                    .context("route missing")?;
                d.status = if route.provider.idempotent()
                    && now.saturating_sub(d.first_attempt_ms.unwrap_or(0)) < 82_800_000
                    && d.attempts < self.config.max_attempts
                {
                    DeliveryStatus::Queued
                } else {
                    DeliveryStatus::Uncertain
                };
            }
        }
        let originals: Vec<_> = next
            .deliveries
            .values()
            .filter(|d| {
                d.generation == 0
                    && d.event.kind.severity() == Severity::Critical
                    && !next.acknowledged.contains_key(&d.event.id)
                    && !next.resolved.contains_key(&d.event.id)
            })
            .cloned()
            .collect();
        for d in originals {
            for generation in 1..=self.config.max_reminders {
                if now.saturating_sub(d.event.observed_at_ms)
                    >= self.config.critical_reminder_ms * u64::from(generation)
                {
                    // Capacity blocks new ingestion, but must not prevent
                    // already-reserved deliveries from draining.
                    if next.deliveries.len() < MAX_DELIVERIES {
                        Self::add_delivery(&mut next, &d.event, &d.route_id, generation, now)?;
                    }
                }
            }
        }
        self.commit(next)?;
        let mut ids: Vec<_> = self
            .state
            .deliveries
            .values()
            .filter(|d| d.next_at_ms <= now)
            .map(|d| d.id.clone())
            .collect();
        ids.sort_by_key(|id| std::cmp::Reverse(self.state.deliveries[id].event.kind.severity()));
        let mut work = 0;
        for id in ids {
            if work >= 32 {
                break;
            }
            let now = clock()?;
            self.checked_time(now)?;
            self.state.last_time_ms = now;
            let mut d = self.state.deliveries[&id].clone();
            let route = self
                .config
                .routes
                .iter()
                .find(|r| r.id == d.route_id)
                .context("route missing")?
                .clone();
            match d.status.clone() {
                DeliveryStatus::Queued => {
                    if (d.generation > 0 && self.state.acknowledged.contains_key(&d.event.id))
                        || self.state.resolved.contains_key(&d.event.id)
                    {
                        d.status = DeliveryStatus::Superseded;
                        let mut next = self.state.clone();
                        next.deliveries.insert(id, d);
                        self.commit(next)?;
                        continue;
                    }
                    if now.saturating_sub(d.event.observed_at_ms) >= self.config.delivery_timeout_ms
                    {
                        d.status = DeliveryStatus::Failed {
                            code: "event_expired".into(),
                        };
                        let mut next = self.state.clone();
                        next.deliveries.insert(id, d);
                        self.commit(next)?;
                        continue;
                    }
                    if route.provider.idempotent()
                        && d.attempts > 0
                        && now.saturating_sub(d.first_attempt_ms.unwrap_or(0)) >= 82_800_000
                    {
                        d.status = DeliveryStatus::Uncertain;
                        let mut next = self.state.clone();
                        next.deliveries.insert(id, d);
                        self.commit(next)?;
                        continue;
                    }
                    if self
                        .state
                        .rate
                        .iter()
                        .filter(|(r, at)| *r == route.id && now.saturating_sub(*at) < 60_000)
                        .count()
                        >= self.config.sends_per_minute as usize
                    {
                        continue;
                    }
                    if d.attempts >= self.config.max_attempts {
                        d.status = DeliveryStatus::Failed {
                            code: "attempts_exhausted".into(),
                        };
                    } else {
                        d.attempts += 1;
                        d.first_attempt_ms.get_or_insert(now);
                        d.status = DeliveryStatus::Sending;
                        let mut next = self.state.clone();
                        next.rate.push((route.id.clone(), now));
                        next.deliveries.insert(id.clone(), d.clone());
                        self.commit(next)?;
                        let outcome = sender.send(&route, &d);
                        let now = clock()?;
                        self.checked_time(now)?;
                        self.state.last_time_ms = now;
                        work += 1;
                        d.status = match outcome {
                            SendResult::Accepted(provider_id) => {
                                DeliveryStatus::Accepted { provider_id }
                            }
                            SendResult::Displayed => DeliveryStatus::Displayed,
                            SendResult::Rejected => DeliveryStatus::Failed {
                                code: "provider_rejected".into(),
                            },
                            SendResult::Retry => {
                                if d.attempts < self.config.max_attempts {
                                    DeliveryStatus::Queued
                                } else {
                                    DeliveryStatus::Failed {
                                        code: "attempts_exhausted".into(),
                                    }
                                }
                            }
                            SendResult::Unknown => {
                                if route.provider.idempotent()
                                    && now.saturating_sub(d.first_attempt_ms.unwrap_or(now))
                                        < 82_800_000
                                    && d.attempts < self.config.max_attempts
                                {
                                    DeliveryStatus::Queued
                                } else {
                                    DeliveryStatus::Uncertain
                                }
                            }
                        };
                        d.next_at_ms = now.saturating_add(
                            self.config
                                .retry_ms
                                .saturating_mul(1_u64 << d.attempts.min(8))
                                .min(300_000),
                        );
                    }
                }
                DeliveryStatus::Accepted { provider_id } => {
                    if now.saturating_sub(d.first_attempt_ms.unwrap_or(0))
                        >= self.config.delivery_timeout_ms
                    {
                        d.status = DeliveryStatus::Uncertain;
                    } else {
                        d.status = match sender.poll(&route, &provider_id) {
                            Receipt::Delivered => DeliveryStatus::Delivered { provider_id },
                            Receipt::Failed => DeliveryStatus::Failed {
                                code: "delivery_failed".into(),
                            },
                            Receipt::Pending | Receipt::Unknown => {
                                DeliveryStatus::Accepted { provider_id }
                            }
                        };
                        let now = clock()?;
                        self.checked_time(now)?;
                        self.state.last_time_ms = now;
                        work += 1;
                        d.next_at_ms = now.saturating_add(self.config.poll_ms);
                    }
                }
                _ => continue,
            }
            let mut next = self.state.clone();
            next.deliveries.insert(id, d);
            self.commit(next)?;
        }
        Ok(())
    }
}
