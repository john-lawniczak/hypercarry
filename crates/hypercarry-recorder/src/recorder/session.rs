use super::{RecorderConfig, RecorderError};
use crate::{normalized::NORMALIZED_SCHEMA_VERSION, raw::RAW_SCHEMA_VERSION};
use std::{path::PathBuf, time::SystemTime};

/// Concrete paths for the independent raw and normalized session layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecorderPaths {
    /// Path to the durable raw JSON-lines capture.
    pub raw_capture: PathBuf,
    /// Path to the rebuildable normalized Parquet projection.
    pub normalized_parquet: PathBuf,
}

impl RecorderPaths {
    pub(super) fn for_session(config: &RecorderConfig, session_id: &str) -> Self {
        let raw_capture = config
            .dataset_root
            .join("raw")
            .join(format!("schema_version={RAW_SCHEMA_VERSION}"))
            .join(format!("network={}", config.network))
            .join(format!("{session_id}.jsonl"));
        let normalized_parquet = config
            .dataset_root
            .join("normalized")
            .join(format!("schema_version={NORMALIZED_SCHEMA_VERSION}"))
            .join(format!("network={}", config.network))
            .join(format!("{session_id}.parquet"));
        Self {
            raw_capture,
            normalized_parquet,
        }
    }

    pub(super) async fn create_parents(&self) -> Result<(), RecorderError> {
        for path in [&self.raw_capture, &self.normalized_parquet] {
            let parent = path.parent().ok_or_else(|| {
                RecorderError::Configuration(format!(
                    "session path {} has no parent directory",
                    path.display()
                ))
            })?;
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(RecorderError::Io)?;
        }
        Ok(())
    }
}

pub(super) fn timestamp_ms() -> Result<i64, RecorderError> {
    let milliseconds = SystemTime::UNIX_EPOCH
        .elapsed()
        .map_err(RecorderError::Clock)?
        .as_millis();
    i64::try_from(milliseconds).map_err(|_| {
        RecorderError::Configuration(format!("unix timestamp {milliseconds}ms does not fit i64"))
    })
}
