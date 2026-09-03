use crate::{
    EXECUTION_SCHEMA_VERSION, ExecutionError, ExecutionMode, LifecycleTransition, OrderState,
    Rejection, RiskDecision, ValidatedOrder, model::validate_correlation_id,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
};

/// Stable append-only journal record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEvent {
    /// Version of the journal record contract.
    pub schema_version: u32,
    /// Monotonic sequence assigned on append.
    pub sequence: u64,
    /// Append time, unix milliseconds.
    pub recorded_at_ms: i64,
    /// Correlation identifier of the originating order.
    pub correlation_id: String,
    /// The recorded event payload.
    pub event: JournalEventKind,
}

/// Secret-free decision and state payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEventKind {
    /// Risk policy allowed the order.
    RiskAllowed,
    /// A structured risk decision was recorded.
    RiskDecisionRecorded {
        /// Whether the order was allowed.
        allowed: bool,
        /// Stable decision code.
        code: String,
        /// Human-readable decision reason.
        reason: String,
    },
    /// Risk policy rejected the order.
    RiskRejected {
        /// The rejection detail.
        rejection: Rejection,
    },
    /// The venue rejected the order.
    VenueRejected {
        /// The rejection detail.
        rejection: Rejection,
    },
    /// The exact allowed action about to be executed.
    ExactAction {
        /// Mode the action ran in.
        mode: ExecutionMode,
        /// The exact validated order.
        order: ValidatedOrder,
    },
    /// A lifecycle state was observed.
    StateObserved {
        /// Observed lifecycle state.
        state: OrderState,
    },
    /// A lifecycle transition was applied.
    LifecycleTransition {
        /// The applied transition.
        transition: LifecycleTransition,
    },
    /// An external event was applied to tracked state.
    ExternalEventApplied {
        /// Source of the external event.
        source: String,
        /// Deduplication identity of the event.
        event_id: String,
    },
}

/// Durable append boundary used by the dry-run pipeline.
pub trait Journal {
    /// Durably appends one event and returns its assigned sequence.
    ///
    /// # Errors
    ///
    /// Returns an error on serialization, I/O, or sequence overflow. Callers
    /// must treat an error as fail-closed.
    fn append(
        &mut self,
        recorded_at_ms: i64,
        correlation_id: &str,
        event: JournalEventKind,
    ) -> Result<JournalEvent, ExecutionError>;
}

/// JSONL journal that validates existing records before continuing a sequence.
pub struct FileJournal {
    writer: BufWriter<File>,
    next_sequence: u64,
}

impl FileJournal {
    /// Opens or creates a journal after validating every existing sequence.
    ///
    /// # Errors
    ///
    /// Returns an error for unreadable files, malformed JSON, unsupported
    /// schema versions, blank lines, or non-contiguous sequences.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ExecutionError> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        file.try_lock_exclusive().map_err(|error| {
            ExecutionError::JournalIo(std::io::Error::new(
                error.kind(),
                format!("journal is already locked by another process: {error}"),
            ))
        })?;
        let next_sequence = next_sequence_from(File::try_clone(&file)?)?;
        Ok(Self {
            writer: BufWriter::new(file),
            next_sequence,
        })
    }
}

impl Journal for FileJournal {
    fn append(
        &mut self,
        recorded_at_ms: i64,
        correlation_id: &str,
        event: JournalEventKind,
    ) -> Result<JournalEvent, ExecutionError> {
        let following_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| ExecutionError::JournalSchema("sequence overflow".to_owned()))?;
        let record = JournalEvent {
            schema_version: EXECUTION_SCHEMA_VERSION,
            sequence: self.next_sequence,
            recorded_at_ms,
            correlation_id: correlation_id.to_owned(),
            event,
        };
        validate_record(&record)?;
        let mut encoded = serde_json::to_vec(&record)?;
        encoded.push(b'\n');
        self.writer.write_all(&encoded)?;
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        self.next_sequence = following_sequence;
        Ok(record)
    }
}

fn next_sequence_from(file: File) -> Result<u64, ExecutionError> {
    let reader = BufReader::new(file);
    let mut expected = 0_u64;
    for (line_index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            return Err(ExecutionError::JournalSchema(format!(
                "blank line at {}",
                line_index + 1
            )));
        }
        let record: JournalEvent = serde_json::from_str(&line)?;
        if record.schema_version != EXECUTION_SCHEMA_VERSION {
            return Err(ExecutionError::JournalSchema(format!(
                "unsupported schema version {} at line {}",
                record.schema_version,
                line_index + 1
            )));
        }
        if record.sequence != expected {
            return Err(ExecutionError::JournalSchema(format!(
                "expected sequence {expected}, found {} at line {}",
                record.sequence,
                line_index + 1
            )));
        }
        validate_record(&record)?;
        expected = expected
            .checked_add(1)
            .ok_or_else(|| ExecutionError::JournalSchema("sequence overflow".to_owned()))?;
    }
    Ok(expected)
}

/// Reads and validates a complete journal without acquiring its writer lock.
///
/// # Errors
///
/// Returns an error for I/O, malformed JSON, invalid records, or sequence gaps.
pub fn read_journal(path: impl AsRef<Path>) -> Result<Vec<JournalEvent>, ExecutionError> {
    let reader = BufReader::new(File::open(path)?);
    let mut records = Vec::new();
    for (line_index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            return Err(ExecutionError::JournalSchema(format!(
                "blank line at {}",
                line_index + 1
            )));
        }
        let record: JournalEvent = serde_json::from_str(&line)?;
        let expected = u64::try_from(records.len())
            .map_err(|_| ExecutionError::JournalSchema("sequence overflow".to_owned()))?;
        if record.schema_version != EXECUTION_SCHEMA_VERSION || record.sequence != expected {
            return Err(ExecutionError::JournalSchema(format!(
                "invalid schema or sequence at line {}",
                line_index + 1
            )));
        }
        validate_record(&record)?;
        records.push(record);
    }
    Ok(records)
}

/// In-memory journal for deterministic tests and callers that explicitly do not persist.
#[derive(Debug, Default)]
pub struct InMemoryJournal {
    /// Appended events in sequence order.
    pub events: Vec<JournalEvent>,
}

impl Journal for InMemoryJournal {
    fn append(
        &mut self,
        recorded_at_ms: i64,
        correlation_id: &str,
        event: JournalEventKind,
    ) -> Result<JournalEvent, ExecutionError> {
        let sequence = u64::try_from(self.events.len())
            .map_err(|_| ExecutionError::JournalSchema("sequence overflow".to_owned()))?;
        let record = JournalEvent {
            schema_version: EXECUTION_SCHEMA_VERSION,
            sequence,
            recorded_at_ms,
            correlation_id: correlation_id.to_owned(),
            event,
        };
        validate_record(&record)?;
        self.events.push(record.clone());
        Ok(record)
    }
}

fn validate_record(record: &JournalEvent) -> Result<(), ExecutionError> {
    validate_correlation_id(&record.correlation_id)
        .map_err(|error| ExecutionError::JournalSchema(error.to_string()))?;
    if record.recorded_at_ms < 0 {
        return Err(ExecutionError::JournalSchema(
            "recorded_at_ms must not precede the Unix epoch".to_owned(),
        ));
    }
    match &record.event {
        JournalEventKind::RiskRejected { rejection }
        | JournalEventKind::VenueRejected { rejection } => {
            if rejection.code.trim().is_empty() || rejection.reason.trim().is_empty() {
                return Err(ExecutionError::JournalSchema(
                    "risk rejection code and reason must not be empty".to_owned(),
                ));
            }
        }
        JournalEventKind::RiskDecisionRecorded { code, reason, .. } => {
            if code.trim().is_empty() || reason.trim().is_empty() {
                return Err(ExecutionError::JournalSchema(
                    "risk decision code and reason must not be empty".to_owned(),
                ));
            }
        }
        JournalEventKind::ExactAction { order, .. } => {
            order
                .validate()
                .map_err(|error| ExecutionError::JournalSchema(error.to_string()))?;
            if order.correlation_id != record.correlation_id {
                return Err(ExecutionError::JournalSchema(
                    "event and order correlation IDs do not match".to_owned(),
                ));
            }
        }
        JournalEventKind::LifecycleTransition { transition } => {
            if transition.client_order_id.to_string().is_empty() {
                return Err(ExecutionError::JournalSchema(
                    "client order identity must not be empty".to_owned(),
                ));
            }
        }
        JournalEventKind::ExternalEventApplied { source, event_id } => {
            if source.trim().is_empty()
                || event_id.trim().is_empty()
                || source.len() > 64
                || event_id.len() > 128
            {
                return Err(ExecutionError::JournalSchema(
                    "external event source/identity must be bounded and non-empty".to_owned(),
                ));
            }
        }
        JournalEventKind::RiskAllowed | JournalEventKind::StateObserved { .. } => {}
    }
    Ok(())
}

impl From<RiskDecision> for JournalEventKind {
    fn from(decision: RiskDecision) -> Self {
        match decision {
            RiskDecision::Allow { code, reason } => Self::RiskDecisionRecorded {
                allowed: true,
                code,
                reason,
            },
            RiskDecision::Reject { code, reason } => Self::RiskRejected {
                rejection: Rejection { code, reason },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn file_journal_appends_and_resumes_monotonic_sequence() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("execution.jsonl");
        let mut journal = FileJournal::open(&path).unwrap();
        journal
            .append(1, "intent-1", JournalEventKind::RiskAllowed)
            .unwrap();
        drop(journal);
        let mut journal = FileJournal::open(&path).unwrap();
        let event = journal
            .append(
                2,
                "intent-1",
                JournalEventKind::StateObserved {
                    state: OrderState::DryRunRecorded,
                },
            )
            .unwrap();
        assert_eq!(event.sequence, 1);
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 2);
    }

    #[test]
    fn file_journal_rejects_a_sequence_gap_before_appending() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("execution.jsonl");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"sequence":2,"recorded_at_ms":1,"correlation_id":"x","event":{"kind":"risk_allowed"}}
"#,
        )
        .unwrap();
        let error = FileJournal::open(path).err().expect("gap must fail closed");
        assert!(error.to_string().contains("expected sequence 0"));
    }

    #[test]
    fn file_journal_rejects_a_second_process_writer() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("execution.jsonl");
        let _first = FileJournal::open(&path).unwrap();
        let error = FileJournal::open(&path)
            .err()
            .expect("second writer must fail closed");
        assert!(error.to_string().contains("already locked"));
    }
}
