//! Connects portfolio observation to the existing notification outbox.
//!
//! This module never sends an alert itself. It only derives secret-free
//! [`hypercarry_notifications::NotificationEvent`] records from an
//! [`crate::observations::Observation`] and appends them to a private,
//! append-only JSON-lines journal in the same archive directory. That journal
//! is an ordinary `hypercarry-notifications` [`hypercarry_notifications::Source`]:
//! point an `Outbox` configuration at `notifications.jsonl` and delivery,
//! reminders, and acknowledgement all use the existing, unchanged pipeline.
//!
//! Two things are tracked so repeated local reads raise an alert only on a
//! real change, never on every poll: per-coin protection status, and whether
//! the last read succeeded. A separate, independent heartbeat check can
//! detect that the collector itself has stopped running, which a crashed or
//! wedged collector could never report about itself.

use crate::observations::Observation;
use hypercarry_core::info::Network;
use hypercarry_notifications::{
    Kind, NotificationEvent, digest, lock_private, read_private, write_private, write_private_bytes,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// Shared `trade_id` for every alert this module emits, distinguishing them
/// from a managed bracket engine's own per-trade alerts.
const TRADE_ID: &str = "portfolio-observation";
/// Synthetic coin label for an account-wide (not per-position) condition.
const ACCOUNT_COIN: &str = "__account__";
/// Synthetic coin label for the independent collector-liveness check.
const COLLECTOR_COIN: &str = "__collector__";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AlertState {
    schema_version: u32,
    account_unknown: bool,
    heartbeat_alerted: bool,
    protected: BTreeMap<String, bool>,
}
impl AlertState {
    const fn fresh() -> Self {
        Self {
            schema_version: 1,
            account_unknown: false,
            heartbeat_alerted: false,
            protected: BTreeMap::new(),
        }
    }
}

fn lock(root: &Path) -> io::Result<File> {
    lock_private(&root.join(".portfolio-alerts.lock")).map_err(|e| io::Error::other(e.to_string()))
}
fn load_state(root: &Path) -> io::Result<AlertState> {
    let path = root.join(".portfolio-alerts-state.json");
    if path.symlink_metadata().is_err() {
        return Ok(AlertState::fresh());
    }
    let bytes = read_private(&path).map_err(|e| io::Error::other(e.to_string()))?;
    let state: AlertState = serde_json::from_slice(&bytes)?;
    if state.schema_version != 1 {
        return Err(io::Error::other(
            "alert state belongs to another schema version",
        ));
    }
    Ok(state)
}
fn save_state(root: &Path, state: &AlertState) -> io::Result<()> {
    write_private(&root.join(".portfolio-alerts-state.json"), state)
        .map_err(|e| io::Error::other(e.to_string()))
}
/// Append each event to the private `notifications.jsonl` source journal.
/// The file only ever grows: existing bytes are preserved and new lines are
/// added before the whole file is atomically replaced, matching the
/// byte-offset cursor that `hypercarry_notifications::Outbox` relies on.
fn append_journal(root: &Path, events: &[NotificationEvent]) -> io::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let path = root.join("notifications.jsonl");
    let mut bytes = if path.symlink_metadata().is_ok() {
        read_private(&path).map_err(|e| io::Error::other(e.to_string()))?
    } else {
        Vec::new()
    };
    for event in events {
        serde_json::to_writer(&mut bytes, &serde_json::json!({ "notification": event }))?;
        bytes.push(b'\n');
    }
    write_private_bytes(&path, &bytes).map_err(|e| io::Error::other(e.to_string()))
}
fn event(
    network: Network,
    coin: &str,
    kind: Kind,
    observed_at_ms: u64,
    remaining: Option<Decimal>,
) -> io::Result<NotificationEvent> {
    let id = digest(&(
        "portfolio-observation-alert-v1",
        network.as_str(),
        coin,
        kind,
        observed_at_ms,
    ))
    .map_err(|e| io::Error::other(e.to_string()))?;
    let event = NotificationEvent {
        schema_version: 1,
        id,
        network: network.as_str().into(),
        trade_id: TRADE_ID.into(),
        coin: coin.into(),
        kind,
        observed_at_ms,
        remaining,
    };
    event
        .validate()
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(event)
}

/// Derive protection/staleness alerts from one observation and durably append
/// any that represent a real change since the previous call for this archive
/// directory. Returns the events actually appended (empty when nothing
/// changed). Call this once per `portfolio --archive-dir` attempt, success or
/// failure, immediately after [`crate::observations::save`].
///
/// Transitions covered: a position losing full reduce-only TP/SL coverage
/// (`ProtectionMissing`), regaining it (`ProtectionVerified`), a previously
/// tracked position disappearing from the snapshot (`PositionClosed`), and
/// the read itself failing (`StateUnknown`). A read succeeding again after a
/// failure clears the tracked state silently, so a later failure raises a
/// fresh alert instead of staying suppressed. Orphan trigger orders are
/// reported by the portfolio output itself and are not separately alerted
/// here.
///
/// # Errors
/// Rejects an unsafe or non-private archive directory, concurrent writers,
/// corrupted alert state, or an observation with neither a snapshot nor a
/// read error.
pub fn record_protection_changes(
    root: &Path,
    observation: &Observation<'_>,
) -> io::Result<Vec<NotificationEvent>> {
    let _lock = lock(root)?;
    let mut state = load_state(root)?;
    let now = observation.completed_at_ms;
    let mut events = Vec::new();
    match (observation.snapshot, observation.read_error) {
        (None, Some(_)) => {
            if !state.account_unknown {
                events.push(event(
                    observation.network,
                    ACCOUNT_COIN,
                    Kind::StateUnknown,
                    now,
                    None,
                )?);
            }
            state.account_unknown = true;
        }
        (Some(snapshot), None) => {
            state.account_unknown = false;
            let mut seen = std::collections::BTreeSet::new();
            for dex in &snapshot.dexs {
                for exit in &dex.exits {
                    seen.insert(exit.coin.clone());
                    let protected = exit.full_pair_observed();
                    match state.protected.get(&exit.coin) {
                        Some(&was) if was == protected => {}
                        Some(&true) | None if !protected => {
                            events.push(event(
                                observation.network,
                                &exit.coin,
                                Kind::ProtectionMissing,
                                now,
                                Some(exit.quantity),
                            )?);
                        }
                        _ => {
                            events.push(event(
                                observation.network,
                                &exit.coin,
                                Kind::ProtectionVerified,
                                now,
                                Some(exit.quantity),
                            )?);
                        }
                    }
                    state.protected.insert(exit.coin.clone(), protected);
                }
            }
            let closed: Vec<String> = state
                .protected
                .keys()
                .filter(|coin| !seen.contains(*coin))
                .cloned()
                .collect();
            for coin in closed {
                events.push(event(
                    observation.network,
                    &coin,
                    Kind::PositionClosed,
                    now,
                    Some(Decimal::ZERO),
                )?);
                state.protected.remove(&coin);
            }
        }
        _ => {
            return Err(io::Error::other(
                "observation must contain exactly one of snapshot or read_error",
            ));
        }
    }
    save_state(root, &state)?;
    append_journal(root, &events)?;
    Ok(events)
}

/// Record that the collector attempted an observation just now. The file's
/// own modification time, not its content, is the liveness signal that
/// [`check_heartbeat`] trusts; a copied or replicated file cannot forge it.
/// The content is retained only as an operator-readable breadcrumb.
///
/// # Errors
/// Rejects an unsafe or non-private archive directory or a filesystem failure.
pub fn touch_heartbeat(root: &Path, network: Network, now_ms: u64) -> io::Result<()> {
    write_private(
        &root.join(".portfolio-heartbeat.json"),
        &serde_json::json!({ "schema_version": 1, "network": network.as_str(), "last_attempt_ms": now_ms }),
    )
    .map_err(|e| io::Error::other(e.to_string()))
}

/// Independently check whether the collector is still running, by comparing
/// the heartbeat file's modification time against `now_ms`. This performs no
/// network or account access and does not depend on the collector process
/// noticing its own failure: a crashed, wedged, or never-started collector
/// simply leaves the file absent or old, which this detects from the outside.
///
/// A missing heartbeat file is reported the same as a stale one. On the
/// transition into staleness, a `HeartbeatLost` alert is appended to the
/// notification journal exactly once; it is not resolved automatically and
/// requires human acknowledgement through the outbox, matching how this
/// project treats every other state-unknown condition. Returns whether the
/// heartbeat is currently stale.
///
/// # Errors
/// Rejects an unsafe or non-private archive directory, concurrent writers, or
/// corrupted alert state.
pub fn check_heartbeat(
    root: &Path,
    network: Network,
    max_age_ms: u64,
    now_ms: u64,
) -> io::Result<bool> {
    let _lock = lock(root)?;
    let mut state = load_state(root)?;
    let path = root.join(".portfolio-heartbeat.json");
    let age_ms = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_symlink() => {
            let modified = metadata.modified()?;
            let modified_ms = modified
                .duration_since(UNIX_EPOCH)
                .map_err(|e| io::Error::other(e.to_string()))?
                .as_millis();
            u64::try_from(modified_ms).map_or(u64::MAX, |m| now_ms.saturating_sub(m))
        }
        _ => u64::MAX,
    };
    let is_stale = age_ms > max_age_ms;
    if is_stale {
        if !state.heartbeat_alerted {
            let alert = event(network, COLLECTOR_COIN, Kind::HeartbeatLost, now_ms, None)?;
            append_journal(root, std::slice::from_ref(&alert))?;
        }
        state.heartbeat_alerted = true;
    } else {
        state.heartbeat_alerted = false;
    }
    save_state(root, &state)?;
    Ok(is_stale)
}

/// Best-effort current time in Unix milliseconds for CLI callers that need
/// one outside an `async` context already producing it.
///
/// # Errors
/// Returns an error if the system clock is set before the Unix epoch.
pub fn now_ms() -> io::Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| io::Error::other(e.to_string()))?
            .as_millis(),
    )
    .map_err(|e| io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests;
