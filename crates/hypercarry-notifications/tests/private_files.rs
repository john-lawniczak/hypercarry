#![cfg(unix)]
// Keep the external mkfifo fixture in its own test process. Forking alongside
// outbox reopen tests can temporarily inherit their flock descriptors until
// exec, even when those descriptors are close-on-exec. Production lock checks
// and immediate competing-writer assertions must remain strict.
use hypercarry_notifications::{lock_private, read_private, write_private};
use serde_json::json;
use std::fs;

#[test]
fn private_reads_and_locks_validate_the_open_inode_and_refuse_nonregular_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let target = dir.path().join("token");
    write_private(&target, &json!({"fixture": true})).unwrap();
    let link = dir.path().join("symlink");
    symlink(&target, &link).unwrap();
    assert!(read_private(&link).is_err());
    assert!(lock_private(&link).is_err());
    fs::remove_file(&link).unwrap();
    fs::hard_link(&target, &link).unwrap();
    assert!(read_private(&link).is_err());
    assert!(lock_private(&link).is_err());
    fs::remove_file(&link).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_private(&target).is_err());
    assert!(lock_private(&target).is_err());
    let fifo = dir.path().join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        read_private(&fifo).is_err(),
        "must reject without blocking for a pipe writer"
    );
    assert!(lock_private(&fifo).is_err());
}
