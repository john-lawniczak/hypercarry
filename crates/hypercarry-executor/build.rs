use sha2::{Digest, Sha256};
use std::{path::Path, process::Command};

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let result = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).trim().to_owned())
}
fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = git(&root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let clean = git(
        &root,
        &["status", "--porcelain", "--untracked-files=normal"],
    )
    .is_some_and(|s| s.is_empty());
    let lock = std::fs::read(root.join("Cargo.lock")).expect("workspace lockfile must be readable");
    println!("cargo:rustc-env=HYPERCARRY_BUILD_COMMIT={source}");
    println!("cargo:rustc-env=HYPERCARRY_BUILD_CLEAN={clean}");
    println!(
        "cargo:rustc-env=HYPERCARRY_BUILD_LOCK={}",
        hex(&Sha256::digest(lock))
    );
    // Re-sample identity whenever workspace source, manifests or review docs change.
    for input in [
        "Cargo.toml",
        "Cargo.lock",
        ".git/HEAD",
        ".git/refs",
        ".git/index",
        "crates",
        "docs",
        "README.md",
        "TODO.md",
        "DEV_STATUS.md",
        "CONTRIBUTING.md",
        "rust-toolchain.toml",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                char::from(DIGITS[usize::from(b >> 4)]),
                char::from(DIGITS[usize::from(b & 15)]),
            ]
        })
        .collect()
}
