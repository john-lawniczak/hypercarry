//! Liveness lease owned by the executor process, never by a free-running timer.
use anyhow::{Context, Result, ensure};
use hypercarry_execution::{DeadManSwitch, FileDeadManSwitch};
use hypercarry_hyperliquid::{Clock, SystemClock};
use hypercarry_mainnet_config::RuntimeConfig;
use serde_json::json;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub struct LeaseGuard {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    path: PathBuf,
}
impl LeaseGuard {
    /// Starts a reviewed liveness lease and waits for actual watchdog arming.
    /// # Errors
    /// Returns an error if the lease cannot be persisted or the watchdog does
    /// not acknowledge protection within 15 seconds.
    pub fn start(runtime: &RuntimeConfig, executor_stop: Arc<AtomicBool>) -> Result<Option<Self>> {
        let Some(path) = runtime.executor_lease.clone() else {
            return Ok(None);
        };
        let binding = runtime.digest()?;
        write(&path, &binding)?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_path = path.clone();
        let thread = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) && !executor_stop.load(Ordering::Relaxed) {
                if write(&worker_path, &binding).is_err() {
                    executor_stop.store(true, Ordering::Relaxed);
                    break;
                }
                thread::sleep(Duration::from_millis(250));
            }
            let _ = fs::remove_file(worker_path);
        });
        let guard = Self {
            stop,
            thread: Some(thread),
            path,
        };
        let watchdog = FileDeadManSwitch::new(&runtime.heartbeat, runtime.heartbeat_timeout_ms)?;
        let started = Instant::now();
        while !watchdog.is_armed(SystemClock.now_ms()?)? {
            ensure!(
                started.elapsed() < Duration::from_secs(15),
                "watchdog did not arm; lease withdrawn"
            );
            ensure!(
                !runtime.kill_switch.exists(),
                "watchdog engaged the kill switch"
            );
            thread::sleep(Duration::from_millis(100));
        }
        Ok(Some(guard))
    }
}
impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}
fn write(path: &Path, binding: &str) -> Result<()> {
    hypercarry_mainnet_config::validate_private_path(path)?;
    let parent = path.parent().context("lease has no parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(
        &mut file,
        &json!({"schema_version":1,"network":"mainnet","integration_config_digest":binding,"observed_at_ms":SystemClock.now_ms()?}),
    )?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
