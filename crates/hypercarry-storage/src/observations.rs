//! Immutable, private account observations. No trading or credential handling.
//! Each completed file is atomically linked into an existing owner-only directory.

use hypercarry_core::{info::Network, portfolio::PortfolioSnapshot};
use serde::Serialize;
use std::{
    io,
    path::{Path, PathBuf},
};

/// A saved account observation or a failed read attempt.
#[derive(Serialize)]
pub struct Observation<'a> {
    /// Archive schema, independent of the portfolio payload schema.
    pub schema_version: u32,
    /// Explicit venue network.
    pub network: Network,
    /// Public trading account (not an API agent address).
    pub account: &'a str,
    /// Local attempt start time, including failed reads.
    pub started_at_ms: u64,
    /// Time the read attempt completed.
    pub completed_at_ms: u64,
    /// Full snapshot when the read completed; absent on read errors.
    pub snapshot: Option<&'a PortfolioSnapshot>,
    /// False or unknown must never be interpreted as protected.
    pub exit_coverage_observed: Option<bool>,
    /// Read failure, distinct from a complete snapshot with missing exits.
    pub read_error: Option<&'a str>,
}

/// Durable publication receipt. The digest detects corruption, not tampering.
#[derive(Debug)]
pub struct Receipt {
    /// Immutable JSON file path.
    pub path: PathBuf,
    /// SHA-256 of the exact file bytes, including its final newline.
    pub sha256: String,
}

/// Save an immutable observation without following final-directory symlinks.
/// The existing directory must be owned by this user with mode 0700 (or stricter).
/// Writers use exclusive temporary files and atomic no-overwrite publication;
/// both file bytes and directory entries are synced before success is returned.
///
/// # Errors
/// Rejects invalid observations, unsafe directory permissions, unsupported
/// platforms and filesystem failures. A failure after publication can leave a
/// completed file; never delete history just because the caller saw an error.
pub fn save(root: &Path, observation: &Observation<'_>) -> io::Result<Receipt> {
    use sha2::{Digest, Sha256};
    if observation.schema_version != 1 || observation.completed_at_ms < observation.started_at_ms {
        return Err(io::Error::other("invalid observation schema or clock"));
    }
    hypercarry_core::account::validate_address(observation.account).map_err(io::Error::other)?;
    match (observation.snapshot, observation.read_error) {
        (Some(s), None)
            if s.account.eq_ignore_ascii_case(observation.account)
                && s.network == observation.network
                && s.schema_version == 2
                && s.started_at_ms >= observation.started_at_ms
                && s.completed_at_ms >= s.started_at_ms
                && s.completed_at_ms <= observation.completed_at_ms
                && observation.exit_coverage_observed == Some(s.exit_coverage_observed()) => {}
        (None, Some(e)) if !e.is_empty() && observation.exit_coverage_observed.is_none() => {}
        _ => return Err(io::Error::other("inconsistent account observation")),
    }
    let mut buffer = LimitedBuffer(Vec::new());
    serde_json::to_writer_pretty(&mut buffer, observation)?;
    std::io::Write::write_all(&mut buffer, b"\n")?;
    let bytes = buffer.0;
    let digest = hex_digest(&Sha256::digest(&bytes));
    let path = publish(root, &bytes, observation.completed_at_ms)?;
    Ok(Receipt {
        path,
        sha256: digest,
    })
}

const MAX_OBSERVATION_BYTES: usize = 16 * 1024 * 1024;
struct LimitedBuffer(Vec<u8>);
impl io::Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_OBSERVATION_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("observation exceeds 16 MiB archive limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                char::from(HEX[(b >> 4) as usize]),
                char::from(HEX[(b & 15) as usize]),
            ]
        })
        .collect()
}

#[cfg(unix)]
fn publish(root: &Path, bytes: &[u8], completed_at_ms: u64) -> io::Result<PathBuf> {
    use rustix::fs::{AtFlags, Mode, OFlags, linkat, open, openat, unlinkat};
    use std::{
        fs::File,
        io::Write,
        os::unix::fs::MetadataExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    // Normalize trailing slash/dot components so O_NOFOLLOW protects the
    // actual final directory, including paths such as "symlink/.".
    let normalized: PathBuf = root.components().collect();
    let dir = File::from(open(
        &normalized,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = dir.metadata()?;
    if metadata.mode() & 0o077 != 0 || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::other(
            "archive directory must be owner-only and owned by the current user",
        ));
    }
    for _ in 0..100 {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "Z-observation-{completed_at_ms}-{}-{sequence}.json",
            std::process::id()
        );
        let temporary = format!("{name}.tmp");
        let fd = match openat(
            &dir,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::EXIST) => continue,
            Err(e) => return Err(e.into()),
        };
        let result = (|| -> io::Result<()> {
            let mut file = File::from(fd);
            file.write_all(bytes)?;
            file.sync_all()?;
            // Both paths are resolved relative to the pinned directory descriptor.
            // linkat fails if the final name already exists; rename would overwrite.
            linkat(
                &dir,
                temporary.as_str(),
                &dir,
                name.as_str(),
                AtFlags::empty(),
            )?;
            dir.sync_all()?;
            Ok(())
        })();
        let cleanup = unlinkat(&dir, temporary.as_str(), AtFlags::empty());
        result?;
        cleanup?;
        dir.sync_all()?;
        return Ok(root.join(name));
    }
    Err(io::Error::other("archive filename reservation exhausted"))
}

#[cfg(not(unix))]
fn publish(_root: &Path, _bytes: &[u8], _completed_at_ms: u64) -> io::Result<PathBuf> {
    Err(io::Error::other(
        "private observation archives currently require Unix",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn failed() -> Observation<'static> {
        Observation {
            schema_version: 1,
            network: Network::Testnet,
            account: "0x1111111111111111111111111111111111111111",
            started_at_ms: 1,
            completed_at_ms: 2,
            snapshot: None,
            exit_coverage_observed: None,
            read_error: Some("fixture outage"),
        }
    }
    #[test]
    fn repeated_failures_are_private_unique_complete_and_hashed() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let a = save(dir.path(), &failed()).unwrap();
        let b = save(dir.path(), &failed()).unwrap();
        assert_ne!(a.path, b.path);
        let bytes = std::fs::read(&a.path).unwrap();
        assert_eq!(a.sha256, hex_digest(&Sha256::digest(&bytes)));
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["snapshot"].is_null());
        assert!(v["exit_coverage_observed"].is_null());
        assert_eq!(
            std::fs::metadata(a.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
    #[test]
    fn archive_rejects_public_directory_symlink_and_inconsistent_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(save(dir.path(), &failed()).is_err());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = dir.path().join("alias");
        symlink(dir.path(), &alias).unwrap();
        assert!(save(&alias, &failed()).is_err());
        assert!(save(&alias.join("."), &failed()).is_err());
        assert!(save(&PathBuf::from(format!("{}/", alias.display())), &failed()).is_err());
        let mut invalid = failed();
        invalid.exit_coverage_observed = Some(true);
        assert!(save(dir.path(), &invalid).is_err());
        invalid = failed();
        invalid.completed_at_ms = 0;
        assert!(save(dir.path(), &invalid).is_err());
    }
    #[test]
    fn complete_unprotected_observation_is_saved_without_claiming_coverage() {
        use hypercarry_core::{
            account::PositionState,
            portfolio::{DexSnapshot, ExitCoverage},
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let positions: PositionState = serde_json::from_value(serde_json::json!({"assetPositions":[{"position":{"coin":"EXAMPLE","szi":"-1","entryPx":"100","leverage":{"type":"isolated","value":1,"rawUsd":"200"},"positionValue":"100","unrealizedPnl":"0","marginUsed":"100","liquidationPx":"195","cumFunding":{}}}],"time":2,"crossMarginSummary":{},"crossMaintenanceMarginUsed":"0"})).unwrap();
        let snapshot = PortfolioSnapshot {
            schema_version: 2,
            network: Network::Testnet,
            account: failed().account.into(),
            scope: "fixture".into(),
            started_at_ms: 1,
            completed_at_ms: 2,
            balances: serde_json::from_value(serde_json::json!({"balances":[]})).unwrap(),
            dexs: vec![DexSnapshot {
                dex: String::new(),
                started_at_ms: 1,
                completed_at_ms: 2,
                positions_before: positions.clone(),
                positions,
                orders: vec![],
                exits: vec![ExitCoverage {
                    coin: "EXAMPLE".into(),
                    quantity: rust_decimal::Decimal::ONE,
                    stop_order_ids: vec![],
                    take_order_ids: vec![],
                }],
                orphan_trigger_ids: vec![],
            }],
        };
        let observation = Observation {
            snapshot: Some(&snapshot),
            read_error: None,
            exit_coverage_observed: Some(false),
            ..failed()
        };
        let receipt = save(dir.path(), &observation).unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(receipt.path).unwrap()).unwrap();
        assert_eq!(v["exit_coverage_observed"], false);
        assert!(v["snapshot"].is_object());
        let wrong_time = Observation {
            started_at_ms: 2,
            ..observation
        };
        assert!(save(dir.path(), &wrong_time).is_err());
        let wrong = Observation {
            account: "0x2222222222222222222222222222222222222222",
            ..observation
        };
        assert!(save(dir.path(), &wrong).is_err());
    }

    #[test]
    fn concurrent_attempts_never_replace_each_other() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| save(dir.path(), &failed()).unwrap()))
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
        });
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 8);
    }
    #[test]
    fn serialization_is_bounded_before_any_file_is_created() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let oversized = "x".repeat(MAX_OBSERVATION_BYTES);
        let observation = Observation {
            read_error: Some(&oversized),
            ..failed()
        };
        assert!(
            save(dir.path(), &observation)
                .unwrap_err()
                .to_string()
                .contains("16 MiB")
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
