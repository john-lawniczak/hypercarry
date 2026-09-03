## Summary

Describe the behavior and why it belongs in hypercarry.

## Safety and compatibility

- [ ] The default CLI remains read-only, or the execution-boundary impact is explained.
- [ ] Financial calculations continue to use exact decimal arithmetic.
- [ ] Public API, CLI, dataset, journal, and evidence compatibility was considered.
- [ ] No credentials, wallet material, or sensitive live artifacts are included.

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- [ ] `cargo test --workspace --all-features --locked`
- [ ] Additional checks and live tests, when applicable, are described below.

