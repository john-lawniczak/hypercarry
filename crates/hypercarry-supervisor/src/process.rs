//! Lifetime-bound process groups for one-shot external helpers.
use std::{
    os::unix::process::CommandExt,
    process::{Child, Command},
};

/// Owns an isolated helper group, including descendants that outlive its leader.
/// The helper must not daemonize or leave this group. OS containment remains the
/// deployment boundary for a malicious executable running as the service user.
pub struct HelperProcess {
    pub child: Child,
}

impl HelperProcess {
    /// Spawn a one-shot helper in a new group. Cleanup runs on every return path,
    /// including a successful leader exit with background descendants.
    /// # Errors
    /// Returns the OS error if the helper cannot be spawned.
    pub fn spawn(command: &mut Command) -> std::io::Result<Self> {
        Ok(Self {
            child: command.process_group(0).spawn()?,
        })
    }
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        if let Ok(raw) = i32::try_from(self.child.id())
            && let Some(pid) = rustix::process::Pid::from_raw(raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
