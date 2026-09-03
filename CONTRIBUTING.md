# Contributing to hypercarry

Thanks for helping improve hypercarry. The project welcomes focused bug fixes,
tests, documentation, and proposals that strengthen its read-only research
workflow or its explicitly isolated execution boundaries.

## Development setup

Install the stable Rust toolchain selected by `rust-toolchain.toml`, clone the
repository, and run:

```sh
cargo test --workspace --all-features --locked
cargo run -p hypercarry-cli -- --help
```

Normal tests are offline. Tests marked `ignored` contact live Hyperliquid
endpoints and must be run deliberately; see `DEV_STATUS.md` for the current
commands and expectations.

## Before opening a pull request

Run the same checks used by CI:

```sh
tools/check-rust-supply-chain.sh
cargo deny check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo build --workspace --all-features --release --locked
```

Keep changes small enough to review, add regression coverage for behavior
changes, and update `DEV_STATUS.md` when capability or verification claims
change. Public contracts and serialized schemas require documentation and
backward-compatibility consideration.

## Safety boundaries

- Do not commit credentials, wallet material, local datasets, or live evidence
  containing account-sensitive data.
- Keep `hypercarry` read-only and execution default-off.
- Do not add a mainnet transport or weaken an execution gate without the
  reviewed evidence required by `TODO.md` and
  `docs/mainnet-release-gate-v1.md`.
- Preserve exact decimal arithmetic for financial values and fail closed on
  ambiguous venue responses or lifecycle state.
- Keep ordinary tests deterministic and independent of live networks.

Security-sensitive findings should follow [SECURITY.md](SECURITY.md), not a
public issue.

## Commit and pull-request notes

Use an imperative, scoped subject when practical, for example
`fix(storage): reject conflicting partition rows`. Explain the user-visible
behavior, safety consequences, and verification performed in the pull request.

