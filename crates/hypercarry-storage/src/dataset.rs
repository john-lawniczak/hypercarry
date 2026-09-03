//! Durable Parquet storage for the settled-funding dataset.

use crate::settled_funding::{
    DATASET_NAME, DECIMAL_SCALE, IngestionProvenance, RequestWindow, SCHEMA_VERSION,
    SettledFundingIdentity, SettledFundingRecord, SourceEndpointClass, StorageContractError,
    encode_partition_value, parquet_schema,
};
use arrow_array::{
    Array, ArrayRef, Decimal128Array, Int32Array, RecordBatch, StringArray,
    TimestampMillisecondArray,
};
use arrow_schema::{ArrowError, Schema};
use hypercarry_core::{
    info::{Network, ParseNetworkError},
    types::{FundingHistoryEntry, TimestampMs},
};
use parquet::{
    arrow::{ArrowWriter, arrow_reader::ParquetRecordBatchReaderBuilder, parquet_to_arrow_schema},
    errors::ParquetError,
    schema::types::SchemaDescriptor,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering as CmpOrdering,
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Canonical file name within each daily Hive partition.
pub const PARTITION_DATA_FILE: &str = "data.parquet";
/// Per-stream resume marker stored above the daily partitions.
pub const CHECKPOINT_FILE: &str = "_checkpoint.json";

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A durable inclusive resume marker for one network/venue/coin stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeCheckpoint {
    /// Frozen settled-funding schema version.
    pub schema_version: u32,
    /// Hyperliquid deployment.
    pub network: Network,
    /// Funding venue.
    pub venue: String,
    /// Market coin symbol.
    pub coin: String,
    /// Last durably committed settlement. Resume requests may include it again.
    pub last_settlement_time_ms: TimestampMs,
    /// Hypercarry build that last advanced this checkpoint.
    pub software_version: String,
}

/// Summary of a dataset commit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommitReport {
    /// Rows presented to the commit before deduplication.
    pub input_rows: usize,
    /// Rows newly persisted after collapsing equivalent duplicates.
    pub rows_added: usize,
    /// Equivalent duplicate rows collapsed during the merge.
    pub duplicates_collapsed: usize,
    /// Daily partition files atomically replaced by the commit.
    pub partitions_written: usize,
    /// Per-stream resume checkpoints advanced by the commit.
    pub checkpoints_written: usize,
}

/// Production settled-funding Parquet dataset rooted at a caller-selected path.
#[derive(Debug, Clone)]
pub struct SettledFundingDataset {
    root: PathBuf,
}

impl SettledFundingDataset {
    /// Create a dataset handle. No files are created until [`Self::commit`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Return the configured storage root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Merge records into canonical daily Parquet partitions and then advance
    /// per-stream checkpoints. Both file types use same-directory atomic rename.
    ///
    /// Duplicate identities with identical market values collapse deterministically.
    /// Conflicting market values fail without replacing the affected partition.
    /// A stale checkpoint is safe: callers resume inclusively and replayed rows dedupe.
    ///
    /// # Errors
    ///
    /// Returns [`DatasetError`] if a record violates the frozen contract, an
    /// existing partition is invalid or conflicts, or durable file I/O fails.
    pub fn commit(&self, records: &[SettledFundingRecord]) -> Result<CommitReport, DatasetError> {
        let mut report = CommitReport {
            input_rows: records.len(),
            ..CommitReport::default()
        };
        let mut by_partition: BTreeMap<PathBuf, Vec<SettledFundingRecord>> = BTreeMap::new();
        for record in records {
            let directory = record.partition_directory()?;
            by_partition
                .entry(self.root.join(directory))
                .or_default()
                .push(record.clone());
        }

        let mut checkpoint_candidates: BTreeMap<StreamIdentity, CheckpointCandidate> =
            BTreeMap::new();
        for (directory, incoming) in by_partition {
            let path = directory.join(PARTITION_DATA_FILE);
            let existing = if path.exists() {
                read_parquet_records(&path)?
            } else {
                Vec::new()
            };
            if existing.iter().any(|record| {
                record
                    .partition_directory()
                    .map_or(true, |partition| self.root.join(partition) != directory)
            }) {
                return Err(DatasetError::InvalidParquet {
                    path,
                    message: "row identity does not match the containing partition".to_owned(),
                });
            }
            let existing_count = existing.len();
            let combined_len = existing_count + incoming.len();
            let merged = merge_records(existing.clone(), incoming)?;
            report.duplicates_collapsed += combined_len - merged.len();
            report.rows_added += merged.len().saturating_sub(existing_count);
            let should_write = !path.exists() || existing != merged;
            if should_write {
                write_parquet_records_atomic(&path, &merged)?;
                report.partitions_written += 1;
            }

            for record in merged {
                let stream = StreamIdentity::from_record(&record);
                let candidate = CheckpointCandidate {
                    last_settlement_time_ms: record.identity.settlement_time,
                    software_version: record.provenance.software_version,
                };
                checkpoint_candidates
                    .entry(stream)
                    .and_modify(|current| {
                        if candidate.last_settlement_time_ms > current.last_settlement_time_ms {
                            *current = candidate.clone();
                        }
                    })
                    .or_insert(candidate);
            }
        }

        // Checkpoints advance only after every affected data partition is durable.
        for (stream, candidate) in checkpoint_candidates {
            if self.advance_checkpoint(&stream, candidate)? {
                report.checkpoints_written += 1;
            }
        }
        Ok(report)
    }

    /// Read and validate a daily partition selected by any record belonging to it.
    ///
    /// # Errors
    ///
    /// Returns [`DatasetError`] if the partition is missing, malformed, or
    /// violates the settled-funding v1 contract.
    pub fn partition_records(
        &self,
        record: &SettledFundingRecord,
    ) -> Result<Vec<SettledFundingRecord>, DatasetError> {
        let path = self
            .root
            .join(record.partition_directory()?)
            .join(PARTITION_DATA_FILE);
        read_parquet_records(&path)
    }

    /// Read every daily partition for one network/venue/coin stream.
    ///
    /// Rows are returned in deterministic identity order. A stream that has
    /// not been written yet returns an empty vector.
    ///
    /// # Errors
    ///
    /// Returns [`DatasetError`] if the stream directory cannot be listed, a
    /// partition is malformed, or duplicate identities conflict.
    pub fn stream_records(
        &self,
        network: Network,
        venue: &str,
        coin: &str,
    ) -> Result<Vec<SettledFundingRecord>, DatasetError> {
        let stream = StreamIdentity {
            network,
            venue: venue.to_owned(),
            coin: coin.to_owned(),
        };
        let directory = self.stream_directory(&stream);
        if !directory.exists() {
            return Ok(Vec::new());
        }

        let entries = fs::read_dir(&directory)
            .map_err(|source| DatasetError::io("list stream directory", &directory, source))?;
        let mut partition_paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| {
                DatasetError::io("read stream directory entry", &directory, source)
            })?;
            let entry_path = entry.path();
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("settlement_date_utc=")
                && entry
                    .file_type()
                    .map_err(|source| {
                        DatasetError::io("inspect stream directory entry", &entry_path, source)
                    })?
                    .is_dir()
            {
                let path = entry_path.join(PARTITION_DATA_FILE);
                if path.exists() {
                    partition_paths.push(path);
                }
            }
        }
        partition_paths.sort();

        let mut records = Vec::new();
        for path in partition_paths {
            let partition_records = read_parquet_records(&path)?;
            if partition_records.iter().any(|record| {
                record.identity.network != network
                    || record.identity.venue != venue
                    || record.identity.coin != coin
            }) {
                return Err(DatasetError::InvalidParquet {
                    path,
                    message: "row identity does not match the containing stream".to_owned(),
                });
            }
            records = merge_records(records, partition_records)?;
        }
        Ok(records)
    }

    /// Read the durable checkpoint for a stream, if one exists.
    ///
    /// # Errors
    ///
    /// Returns [`DatasetError`] if the checkpoint cannot be read, decoded, or
    /// does not match the requested stream identity.
    pub fn resume_checkpoint(
        &self,
        network: Network,
        venue: &str,
        coin: &str,
    ) -> Result<Option<ResumeCheckpoint>, DatasetError> {
        let stream = StreamIdentity {
            network,
            venue: venue.to_owned(),
            coin: coin.to_owned(),
        };
        let path = self.checkpoint_path(&stream);
        if !path.exists() {
            return Ok(None);
        }
        let file = File::open(&path)
            .map_err(|source| DatasetError::io("open checkpoint", &path, source))?;
        let checkpoint: ResumeCheckpoint =
            serde_json::from_reader(file).map_err(|source| DatasetError::Json {
                path: path.clone(),
                source,
            })?;
        if checkpoint.schema_version != SCHEMA_VERSION
            || checkpoint.network != network
            || checkpoint.venue != venue
            || checkpoint.coin != coin
        {
            return Err(DatasetError::InvalidCheckpoint {
                path,
                message: "checkpoint identity does not match its storage path".to_owned(),
            });
        }
        Ok(Some(checkpoint))
    }

    fn advance_checkpoint(
        &self,
        stream: &StreamIdentity,
        candidate: CheckpointCandidate,
    ) -> Result<bool, DatasetError> {
        if let Some(existing) =
            self.resume_checkpoint(stream.network, &stream.venue, &stream.coin)?
            && existing.last_settlement_time_ms >= candidate.last_settlement_time_ms
        {
            return Ok(false);
        }
        let checkpoint = ResumeCheckpoint {
            schema_version: SCHEMA_VERSION,
            network: stream.network,
            venue: stream.venue.clone(),
            coin: stream.coin.clone(),
            last_settlement_time_ms: candidate.last_settlement_time_ms,
            software_version: candidate.software_version,
        };
        let path = self.checkpoint_path(stream);
        let mut atomic = AtomicFile::new(&path)?;
        serde_json::to_writer_pretty(atomic.file_mut(), &checkpoint).map_err(|source| {
            DatasetError::Json {
                path: path.clone(),
                source,
            }
        })?;
        atomic
            .file_mut()
            .write_all(b"\n")
            .map_err(|source| DatasetError::io("write checkpoint", &path, source))?;
        atomic.commit()?;
        Ok(true)
    }

    fn checkpoint_path(&self, stream: &StreamIdentity) -> PathBuf {
        self.stream_directory(stream).join(CHECKPOINT_FILE)
    }

    fn stream_directory(&self, stream: &StreamIdentity) -> PathBuf {
        self.root
            .join(DATASET_NAME)
            .join(format!("schema_version={SCHEMA_VERSION}"))
            .join(format!("network={}", stream.network))
            .join(format!("venue={}", encode_partition_value(&stream.venue)))
            .join(format!("coin={}", encode_partition_value(&stream.coin)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamIdentity {
    network: Network,
    venue: String,
    coin: String,
}

impl Ord for StreamIdentity {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        (
            self.network.as_str(),
            self.venue.as_str(),
            self.coin.as_str(),
        )
            .cmp(&(
                other.network.as_str(),
                other.venue.as_str(),
                other.coin.as_str(),
            ))
    }
}

impl PartialOrd for StreamIdentity {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl StreamIdentity {
    fn from_record(record: &SettledFundingRecord) -> Self {
        Self {
            network: record.identity.network,
            venue: record.identity.venue.clone(),
            coin: record.identity.coin.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct CheckpointCandidate {
    last_settlement_time_ms: TimestampMs,
    software_version: String,
}

fn merge_records(
    existing: Vec<SettledFundingRecord>,
    incoming: Vec<SettledFundingRecord>,
) -> Result<Vec<SettledFundingRecord>, DatasetError> {
    let mut rows = BTreeMap::<SettledFundingIdentity, SettledFundingRecord>::new();
    for record in existing.into_iter().chain(incoming) {
        match rows.get_mut(&record.identity) {
            None => {
                rows.insert(record.identity.clone(), record);
            }
            Some(current) => {
                if current.funding_rate != record.funding_rate || current.premium != record.premium
                {
                    return Err(DatasetError::ConflictingDuplicate {
                        identity: record.identity,
                    });
                }
                if provenance_key(&record) < provenance_key(current) {
                    *current = record;
                }
            }
        }
    }
    Ok(rows.into_values().collect())
}

fn provenance_key(record: &SettledFundingRecord) -> (i64, u8, &str, i64, i64) {
    let endpoint_rank = match record.provenance.source_endpoint_class {
        SourceEndpointClass::Official => 0,
        SourceEndpointClass::Development => 1,
    };
    (
        record.provenance.ingestion_time.as_i64(),
        endpoint_rank,
        record.provenance.software_version.as_str(),
        record.provenance.request_window.start().as_i64(),
        record.provenance.request_window.end().as_i64(),
    )
}

fn arrow_schema() -> Result<Arc<Schema>, DatasetError> {
    let descriptor = SchemaDescriptor::new(parquet_schema()?);
    parquet_to_arrow_schema(&descriptor, None)
        .map(Arc::new)
        .map_err(|source| DatasetError::Parquet {
            path: PathBuf::from("<schema>"),
            source,
        })
}

fn scaled_decimal(value: Decimal) -> i128 {
    let mut value = value;
    value.rescale(DECIMAL_SCALE);
    value.mantissa()
}

fn records_to_batch(records: &[SettledFundingRecord]) -> Result<RecordBatch, DatasetError> {
    let schema = arrow_schema()?;
    let scale = i8::try_from(DECIMAL_SCALE).expect("frozen decimal scale fits i8");
    let funding_rate = Decimal128Array::from_iter_values(
        records
            .iter()
            .map(|record| scaled_decimal(record.funding_rate)),
    )
    .with_precision_and_scale(38, scale)?;
    let premium = Decimal128Array::from_iter_values(
        records.iter().map(|record| scaled_decimal(record.premium)),
    )
    .with_precision_and_scale(38, scale)?;
    let columns: Vec<ArrayRef> = vec![
        Arc::new(Int32Array::from_iter_values(records.iter().map(|record| {
            i32::try_from(record.schema_version).expect("schema version fits i32")
        }))),
        Arc::new(StringArray::from_iter_values(
            records
                .iter()
                .map(|record| record.provenance.software_version.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            records
                .iter()
                .map(|record| record.identity.network.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            records.iter().map(|record| record.identity.venue.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            records.iter().map(|record| record.identity.coin.as_str()),
        )),
        Arc::new(
            TimestampMillisecondArray::from_iter_values(
                records
                    .iter()
                    .map(|record| record.identity.settlement_time.as_i64()),
            )
            .with_timezone("UTC"),
        ),
        Arc::new(funding_rate),
        Arc::new(premium),
        Arc::new(StringArray::from_iter_values(
            records
                .iter()
                .map(|record| record.provenance.source_endpoint_class.as_str()),
        )),
        Arc::new(
            TimestampMillisecondArray::from_iter_values(
                records
                    .iter()
                    .map(|record| record.provenance.ingestion_time.as_i64()),
            )
            .with_timezone("UTC"),
        ),
        Arc::new(
            TimestampMillisecondArray::from_iter_values(
                records
                    .iter()
                    .map(|record| record.provenance.request_window.start().as_i64()),
            )
            .with_timezone("UTC"),
        ),
        Arc::new(
            TimestampMillisecondArray::from_iter_values(
                records
                    .iter()
                    .map(|record| record.provenance.request_window.end().as_i64()),
            )
            .with_timezone("UTC"),
        ),
    ];
    RecordBatch::try_new(schema, columns).map_err(DatasetError::Arrow)
}

fn write_parquet_records_atomic(
    path: &Path,
    records: &[SettledFundingRecord],
) -> Result<(), DatasetError> {
    let batch = records_to_batch(records)?;
    let mut atomic = AtomicFile::new(path)?;
    {
        let mut writer =
            ArrowWriter::try_new(atomic.file_mut(), batch.schema(), None).map_err(|source| {
                DatasetError::Parquet {
                    path: path.to_owned(),
                    source,
                }
            })?;
        writer
            .write(&batch)
            .map_err(|source| DatasetError::Parquet {
                path: path.to_owned(),
                source,
            })?;
        writer.close().map_err(|source| DatasetError::Parquet {
            path: path.to_owned(),
            source,
        })?;
    }
    atomic.commit()
}

fn read_parquet_records(path: &Path) -> Result<Vec<SettledFundingRecord>, DatasetError> {
    let file = File::open(path)
        .map_err(|source| DatasetError::io("open Parquet partition", path, source))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|source| DatasetError::Parquet {
            path: path.to_owned(),
            source,
        })?
        .build()
        .map_err(|source| DatasetError::Parquet {
            path: path.to_owned(),
            source,
        })?;
    let expected_schema = arrow_schema()?;
    let mut records = Vec::new();
    for batch in reader {
        let batch = batch.map_err(DatasetError::Arrow)?;
        if batch.schema().as_ref() != expected_schema.as_ref() {
            return Err(DatasetError::InvalidParquet {
                path: path.to_owned(),
                message: "Arrow schema does not match settled-funding v1".to_owned(),
            });
        }
        records.extend(batch_to_records(path, &batch)?);
    }
    let unique_partitions: BTreeSet<_> = records
        .iter()
        .map(SettledFundingRecord::partition_directory)
        .collect::<Result<_, _>>()?;
    if unique_partitions.len() > 1 {
        return Err(DatasetError::InvalidParquet {
            path: path.to_owned(),
            message: "file contains rows from multiple daily partitions".to_owned(),
        });
    }
    Ok(records)
}

fn batch_to_records(
    path: &Path,
    batch: &RecordBatch,
) -> Result<Vec<SettledFundingRecord>, DatasetError> {
    if batch
        .columns()
        .iter()
        .any(|column| column.null_count() != 0)
    {
        return Err(DatasetError::InvalidParquet {
            path: path.to_owned(),
            message: "required settled-funding columns contain null values".to_owned(),
        });
    }
    let versions = typed_column::<Int32Array>(path, batch, 0, "schema_version")?;
    let software = typed_column::<StringArray>(path, batch, 1, "software_version")?;
    let networks = typed_column::<StringArray>(path, batch, 2, "network")?;
    let venues = typed_column::<StringArray>(path, batch, 3, "venue")?;
    let coins = typed_column::<StringArray>(path, batch, 4, "coin")?;
    let settlements =
        typed_column::<TimestampMillisecondArray>(path, batch, 5, "settlement_time_ms")?;
    let rates = typed_column::<Decimal128Array>(path, batch, 6, "funding_rate")?;
    let premiums = typed_column::<Decimal128Array>(path, batch, 7, "premium")?;
    let endpoints = typed_column::<StringArray>(path, batch, 8, "source_endpoint_class")?;
    let ingestions =
        typed_column::<TimestampMillisecondArray>(path, batch, 9, "ingestion_time_ms")?;
    let starts =
        typed_column::<TimestampMillisecondArray>(path, batch, 10, "request_start_time_ms")?;
    let ends = typed_column::<TimestampMillisecondArray>(path, batch, 11, "request_end_time_ms")?;
    let mut records = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        if versions.value(row) != i32::try_from(SCHEMA_VERSION).expect("schema version fits") {
            return Err(DatasetError::InvalidParquet {
                path: path.to_owned(),
                message: format!(
                    "row {row} has unsupported schema version {}",
                    versions.value(row)
                ),
            });
        }
        let network = Network::from_str(networks.value(row))?;
        let source_endpoint_class = match endpoints.value(row) {
            "official" => SourceEndpointClass::Official,
            "development" => SourceEndpointClass::Development,
            value => {
                return Err(DatasetError::InvalidParquet {
                    path: path.to_owned(),
                    message: format!("row {row} has unknown endpoint class {value:?}"),
                });
            }
        };
        let request_window = RequestWindow::new(
            TimestampMs::new(starts.value(row)),
            TimestampMs::new(ends.value(row)),
        )?;
        records.push(SettledFundingRecord::from_history_entry(
            network,
            venues.value(row),
            FundingHistoryEntry {
                coin: coins.value(row).to_owned(),
                funding_rate: Decimal::try_from_i128_with_scale(rates.value(row), DECIMAL_SCALE)
                    .map_err(|error| DatasetError::InvalidParquet {
                        path: path.to_owned(),
                        message: format!("row {row} funding rate is invalid: {error}"),
                    })?,
                premium: Decimal::try_from_i128_with_scale(premiums.value(row), DECIMAL_SCALE)
                    .map_err(|error| DatasetError::InvalidParquet {
                        path: path.to_owned(),
                        message: format!("row {row} premium is invalid: {error}"),
                    })?,
                time: TimestampMs::new(settlements.value(row)),
            },
            IngestionProvenance {
                source_endpoint_class,
                ingestion_time: TimestampMs::new(ingestions.value(row)),
                request_window,
                software_version: software.value(row).to_owned(),
            },
        )?);
    }
    Ok(records)
}

fn typed_column<'a, T: 'static>(
    path: &Path,
    batch: &'a RecordBatch,
    index: usize,
    name: &'static str,
) -> Result<&'a T, DatasetError> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| DatasetError::InvalidParquet {
            path: path.to_owned(),
            message: format!("column {name} has the wrong Arrow type"),
        })
}

struct AtomicFile {
    target: PathBuf,
    temporary: PathBuf,
    file: Option<File>,
}

impl AtomicFile {
    fn new(target: &Path) -> Result<Self, DatasetError> {
        let parent = target
            .parent()
            .ok_or_else(|| DatasetError::InvalidPath(target.to_owned()))?;
        fs::create_dir_all(parent)
            .map_err(|source| DatasetError::io("create parent directory", parent, source))?;
        for _ in 0..100 {
            let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let file_name = target
                .file_name()
                .ok_or_else(|| DatasetError::InvalidPath(target.to_owned()))?;
            let temporary = parent.join(format!(
                ".{}.tmp-{}-{sequence}",
                file_name.to_string_lossy(),
                std::process::id()
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => {
                    return Ok(Self {
                        target: target.to_owned(),
                        temporary,
                        file: Some(file),
                    });
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(DatasetError::io(
                        "create temporary file",
                        &temporary,
                        source,
                    ));
                }
            }
        }
        Err(DatasetError::InvalidPath(target.to_owned()))
    }

    fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("atomic file is open until commit")
    }

    fn commit(mut self) -> Result<(), DatasetError> {
        let file = self.file.take().expect("atomic file is open until commit");
        file.sync_all()
            .map_err(|source| DatasetError::io("sync temporary file", &self.temporary, source))?;
        drop(file);
        fs::rename(&self.temporary, &self.target)
            .map_err(|source| DatasetError::io("replace target file", &self.target, source))?;
        sync_parent_directory(&self.target)?;
        Ok(())
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temporary);
    }
}

#[cfg(unix)]
fn sync_parent_directory(target: &Path) -> Result<(), DatasetError> {
    let parent = target
        .parent()
        .ok_or_else(|| DatasetError::InvalidPath(target.to_owned()))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| DatasetError::io("sync parent directory", parent, source))
}

#[cfg(not(unix))]
fn sync_parent_directory(_target: &Path) -> Result<(), DatasetError> {
    Ok(())
}

/// A durable dataset read, merge, validation, or commit failure.
#[derive(Debug)]
pub enum DatasetError {
    /// A filesystem operation failed.
    Io {
        /// Filesystem operation that failed.
        operation: &'static str,
        /// Path the operation targeted.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// Reading or writing a Parquet partition failed.
    Parquet {
        /// Partition file involved in the failure.
        path: PathBuf,
        /// Underlying Parquet error.
        source: ParquetError,
    },
    /// Converting rows to or from Arrow arrays failed.
    Arrow(ArrowError),
    /// Serializing or deserializing a checkpoint failed.
    Json {
        /// Checkpoint file involved in the failure.
        path: PathBuf,
        /// Underlying JSON error.
        source: serde_json::Error,
    },
    /// A row violated the settled-funding schema contract.
    Contract(StorageContractError),
    /// A stored network identifier could not be parsed.
    Network(ParseNetworkError),
    /// A dataset file path was outside the expected partition layout.
    InvalidPath(PathBuf),
    /// A Parquet partition failed structural validation on read.
    InvalidParquet {
        /// Partition file that failed validation.
        path: PathBuf,
        /// Human-readable reason for rejection.
        message: String,
    },
    /// A checkpoint file failed structural validation on read.
    InvalidCheckpoint {
        /// Checkpoint file that failed validation.
        path: PathBuf,
        /// Human-readable reason for rejection.
        message: String,
    },
    /// Two rows shared one identity but carried different values.
    ConflictingDuplicate {
        /// Identity shared by the conflicting rows.
        identity: SettledFundingIdentity,
    },
}

impl DatasetError {
    fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_owned(),
            source,
        }
    }
}

impl fmt::Display for DatasetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation, path, ..
            } => write!(formatter, "failed to {operation} {}", path.display()),
            Self::Parquet { path, .. } => {
                write!(formatter, "Parquet operation failed for {}", path.display())
            }
            Self::Arrow(error) => write!(formatter, "Arrow conversion failed: {error}"),
            Self::Json { path, .. } => {
                write!(formatter, "checkpoint JSON failed for {}", path.display())
            }
            Self::Contract(error) => write!(formatter, "settled-funding contract failed: {error}"),
            Self::Network(error) => write!(formatter, "invalid stored network: {error}"),
            Self::InvalidPath(path) => {
                write!(formatter, "invalid dataset file path {}", path.display())
            }
            Self::InvalidParquet { path, message } => write!(
                formatter,
                "invalid settled-funding Parquet {}: {message}",
                path.display()
            ),
            Self::InvalidCheckpoint { path, message } => write!(
                formatter,
                "invalid checkpoint {}: {message}",
                path.display()
            ),
            Self::ConflictingDuplicate { identity } => write!(
                formatter,
                "conflicting duplicate for {}/{}/{} at {}ms",
                identity.network,
                identity.venue,
                identity.coin,
                identity.settlement_time.as_i64()
            ),
        }
    }
}

impl Error for DatasetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parquet { source, .. } => Some(source),
            Self::Arrow(source) => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Contract(source) => Some(source),
            Self::Network(source) => Some(source),
            Self::InvalidPath(_)
            | Self::InvalidParquet { .. }
            | Self::InvalidCheckpoint { .. }
            | Self::ConflictingDuplicate { .. } => None,
        }
    }
}

impl From<StorageContractError> for DatasetError {
    fn from(source: StorageContractError) -> Self {
        Self::Contract(source)
    }
}

impl From<ParquetError> for DatasetError {
    fn from(source: ParquetError) -> Self {
        Self::Parquet {
            path: PathBuf::from("<schema>"),
            source,
        }
    }
}

impl From<ArrowError> for DatasetError {
    fn from(source: ArrowError) -> Self {
        Self::Arrow(source)
    }
}

impl From<ParseNetworkError> for DatasetError {
    fn from(source: ParseNetworkError) -> Self {
        Self::Network(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, str::FromStr};

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "hypercarry-storage-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("unique test directory is created");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn decimal(value: &str) -> Decimal {
        Decimal::from_str(value).expect("valid test decimal")
    }

    fn record(
        time: i64,
        funding_rate: &str,
        ingestion_time: i64,
        endpoint: SourceEndpointClass,
    ) -> SettledFundingRecord {
        SettledFundingRecord::from_history_entry(
            Network::Testnet,
            "hyperliquid",
            FundingHistoryEntry {
                coin: "BTC".to_owned(),
                funding_rate: decimal(funding_rate),
                premium: decimal("0.00002"),
                time: TimestampMs::new(time),
            },
            IngestionProvenance {
                source_endpoint_class: endpoint,
                ingestion_time: TimestampMs::new(ingestion_time),
                request_window: RequestWindow::new(
                    TimestampMs::new(time - 1_000),
                    TimestampMs::new(time + 1_000),
                )
                .expect("valid window"),
                software_version: "0.0.0-test".to_owned(),
            },
        )
        .expect("valid record")
    }

    #[test]
    fn commit_round_trips_real_parquet_and_writes_checkpoint() {
        let directory = TestDirectory::new();
        let dataset = SettledFundingDataset::new(&directory.0);
        let first = record(
            1_683_849_600_048,
            "0.00001",
            1_683_849_700_000,
            SourceEndpointClass::Official,
        );
        let second = record(
            1_683_853_200_048,
            "0.00002",
            1_683_853_300_000,
            SourceEndpointClass::Official,
        );

        let report = dataset
            .commit(&[second.clone(), first.clone()])
            .expect("commit succeeds");
        assert_eq!(
            report,
            CommitReport {
                input_rows: 2,
                rows_added: 2,
                duplicates_collapsed: 0,
                partitions_written: 1,
                checkpoints_written: 1,
            }
        );
        assert_eq!(
            dataset.partition_records(&first).expect("partition reads"),
            vec![first, second.clone()]
        );
        assert_eq!(
            dataset
                .resume_checkpoint(Network::Testnet, "hyperliquid", "BTC")
                .expect("checkpoint reads")
                .expect("checkpoint exists")
                .last_settlement_time_ms,
            second.identity.settlement_time
        );

        let parquet_path = directory
            .0
            .join(second.partition_directory().expect("partition"))
            .join(PARTITION_DATA_FILE);
        let bytes = fs::read(parquet_path).expect("Parquet file reads");
        assert_eq!(&bytes[..4], b"PAR1");
        assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");
    }

    #[test]
    fn overlapping_commits_dedupe_sort_and_select_canonical_provenance() {
        let directory = TestDirectory::new();
        let dataset = SettledFundingDataset::new(&directory.0);
        let later = record(
            1_683_849_600_048,
            "0.00001",
            1_683_849_900_000,
            SourceEndpointClass::Development,
        );
        let earlier = record(
            1_683_849_600_048,
            "0.00001",
            1_683_849_800_000,
            SourceEndpointClass::Official,
        );
        let earlier_development = record(
            1_683_849_600_048,
            "0.00001",
            1_683_849_800_000,
            SourceEndpointClass::Development,
        );
        let next = record(
            1_683_853_200_048,
            "0.00002",
            1_683_853_300_000,
            SourceEndpointClass::Official,
        );
        dataset.commit(&[later]).expect("initial commit succeeds");

        let report = dataset
            .commit(&[next.clone(), earlier_development, earlier.clone()])
            .expect("overlap commit succeeds");
        assert_eq!(report.rows_added, 1);
        assert_eq!(report.duplicates_collapsed, 2);
        assert_eq!(report.partitions_written, 1);
        assert_eq!(
            dataset
                .partition_records(&earlier)
                .expect("partition reads"),
            vec![earlier.clone(), next]
        );

        let repeat = dataset.commit(&[earlier]).expect("repeat commit succeeds");
        assert_eq!(repeat.rows_added, 0);
        assert_eq!(repeat.duplicates_collapsed, 1);
        assert_eq!(repeat.partitions_written, 0);
        assert_eq!(repeat.checkpoints_written, 0);
    }

    #[test]
    fn conflicting_duplicate_does_not_replace_existing_partition() {
        let directory = TestDirectory::new();
        let dataset = SettledFundingDataset::new(&directory.0);
        let original = record(
            1_683_849_600_048,
            "0.00001",
            1_683_849_700_000,
            SourceEndpointClass::Official,
        );
        let conflict = record(
            1_683_849_600_048,
            "0.00009",
            1_683_849_800_000,
            SourceEndpointClass::Official,
        );
        dataset
            .commit(std::slice::from_ref(&original))
            .expect("initial commit succeeds");

        let error = dataset
            .commit(&[conflict])
            .expect_err("conflicting market values fail");
        assert!(matches!(error, DatasetError::ConflictingDuplicate { .. }));
        assert_eq!(
            dataset
                .partition_records(&original)
                .expect("original remains"),
            vec![original]
        );
    }

    #[test]
    fn checkpoint_never_regresses_when_old_rows_are_replayed() {
        let directory = TestDirectory::new();
        let dataset = SettledFundingDataset::new(&directory.0);
        let old = record(
            1_683_849_600_048,
            "0.00001",
            1_683_849_700_000,
            SourceEndpointClass::Official,
        );
        let newer = record(
            1_683_853_200_048,
            "0.00002",
            1_683_853_300_000,
            SourceEndpointClass::Official,
        );
        dataset
            .commit(std::slice::from_ref(&newer))
            .expect("new row commits");
        let report = dataset
            .commit(std::slice::from_ref(&old))
            .expect("old row commits");

        assert_eq!(report.checkpoints_written, 0);
        assert_eq!(
            dataset
                .resume_checkpoint(Network::Testnet, "hyperliquid", "BTC")
                .expect("checkpoint reads")
                .expect("checkpoint exists")
                .last_settlement_time_ms,
            newer.identity.settlement_time
        );
    }

    #[test]
    fn stream_reader_orders_daily_partitions_and_allows_a_missing_stream() {
        let directory = TestDirectory::new();
        let dataset = SettledFundingDataset::new(&directory.0);
        let first = record(
            1_683_849_600_048,
            "0.00001",
            1_683_849_700_000,
            SourceEndpointClass::Official,
        );
        let next_day = record(
            1_683_936_000_048,
            "0.00002",
            1_683_936_100_000,
            SourceEndpointClass::Official,
        );
        dataset
            .commit(&[next_day.clone(), first.clone()])
            .expect("both daily partitions commit");

        assert_eq!(
            dataset
                .stream_records(Network::Testnet, "hyperliquid", "BTC")
                .expect("stream reads"),
            vec![first, next_day]
        );
        assert!(
            dataset
                .stream_records(Network::Testnet, "hyperliquid", "ETH")
                .expect("missing stream is empty")
                .is_empty()
        );
    }

    #[test]
    fn abandoned_checkpoint_write_preserves_last_durable_resume_point() {
        let directory = TestDirectory::new();
        let dataset = SettledFundingDataset::new(&directory.0);
        let durable = record(
            1_683_849_600_048,
            "0.00001",
            1_683_849_700_000,
            SourceEndpointClass::Official,
        );
        dataset
            .commit(std::slice::from_ref(&durable))
            .expect("durable checkpoint commits");
        let stream = StreamIdentity::from_record(&durable);
        let target = dataset.checkpoint_path(&stream);
        {
            let mut atomic = AtomicFile::new(&target).expect("temporary file opens");
            atomic
                .file_mut()
                .write_all(b"{\"truncated\":")
                .expect("temporary write succeeds");
        }

        assert_eq!(
            dataset
                .resume_checkpoint(Network::Testnet, "hyperliquid", "BTC")
                .expect("durable checkpoint remains readable")
                .expect("checkpoint exists")
                .last_settlement_time_ms,
            durable.identity.settlement_time
        );
        let checkpoint_directory = target.parent().expect("checkpoint has a parent");
        assert!(
            fs::read_dir(checkpoint_directory)
                .expect("checkpoint directory reads")
                .all(|entry| !entry
                    .expect("directory entry reads")
                    .file_name()
                    .to_string_lossy()
                    .starts_with('.'))
        );
    }
}
