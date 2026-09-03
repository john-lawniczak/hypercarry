use crate::raw::{RawCaptureError, RawCaptureRecord, RawEvent};
use std::{
    fs::File,
    io::{BufRead, BufReader, Lines},
    path::Path,
};

/// Event yielded by deterministic local replay in capture-file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayEvent {
    /// A captured market-data frame.
    Frame(RawCaptureRecord),
    /// A captured connection boundary.
    Disconnect(RawCaptureRecord),
    /// A captured stale-connection recycle.
    Stale(RawCaptureRecord),
}

/// Offline transport over a versioned raw JSON-lines capture.
#[derive(Debug)]
pub struct ReplayTransport {
    lines: Lines<BufReader<File>>,
    line_number: usize,
}

impl ReplayTransport {
    /// Open a raw session for constant-memory replay without contacting a network.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the session cannot be opened. Individual schema
    /// and JSON errors retain their line number when the iterator reaches them.
    pub fn open(path: &Path) -> Result<Self, ReplayError> {
        let file = File::open(path).map_err(ReplayError::Io)?;
        Ok(Self {
            lines: BufReader::new(file).lines(),
            line_number: 0,
        })
    }
}

impl Iterator for ReplayTransport {
    type Item = Result<ReplayEvent, ReplayError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let line = self.lines.next()?;
            self.line_number += 1;
            let line = match line {
                Ok(line) => line,
                Err(error) => return Some(Err(ReplayError::Io(error))),
            };
            if line.trim().is_empty() {
                continue;
            }
            let record = match RawCaptureRecord::from_json_line(&line) {
                Ok(record) => record,
                Err(source) => {
                    return Some(Err(ReplayError::Record {
                        line: self.line_number,
                        source,
                    }));
                }
            };
            let event = match record.event {
                RawEvent::Frame { .. } => ReplayEvent::Frame(record),
                RawEvent::Disconnect { .. } => ReplayEvent::Disconnect(record),
                RawEvent::Stale { .. } => ReplayEvent::Stale(record),
            };
            return Some(Ok(event));
        }
    }
}

/// Local replay input failure with an actionable line number.
#[derive(Debug)]
pub enum ReplayError {
    /// Reading the capture file failed.
    Io(std::io::Error),
    /// A capture line failed to decode.
    Record {
        /// One-based line number of the offending record.
        line: usize,
        /// Underlying raw capture error.
        source: RawCaptureError,
    },
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "could not read raw replay: {error}"),
            Self::Record { line, source } => {
                write!(formatter, "raw replay line {line} is invalid: {source}")
            }
        }
    }
}

impl std::error::Error for ReplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Record { source, .. } => Some(source),
        }
    }
}
