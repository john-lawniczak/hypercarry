# Hypercarry development status

Last reviewed: **2026-11-22**

## Implemented capabilities

- Read-only funding collection, prediction, account observation and scoped
  Unified-USDC accounting. Portfolio observation requires complete, fresh DEX
  coverage; ledger reports preserve missing-history and valuation gaps.
- Default-off Rust testnet and mainnet execution components, persistent journals,
  bounded external signing, recovery and operational health contracts.
- Local testnet bracket planning, a locally unlocked agent signer, full/partial
  reduce-only closes, partial-fill protection repair, exit amendment and
  journal-derived recovery policies. Account reservations and nonce state persist
  across restart; uncertain submissions must be reconciled before another entry.
- A separate, default-off attended mainnet bracket adapter. It binds a clean
  candidate, six reviewed evidence scenarios, three distinct credentialed testnet
  sessions and an exact fresh plan. Its narrow scope is native isolated 1x with
  a $20 planned entry bound and dedicated mainnet agent. It requires manual
  fallback and does not provide unattended availability.
- Notification outboxes, delivery adapters and host/service deployment templates.
  Building these components does not install services or establish real alert
  delivery, independent recovery, custody or human review.

## Partial-close minimum validation

Partial IOC closes require rounded quantity × approved limit to be at least
10 USDC. The planner and execution preflight reject undersized partial orders;
the final action check repeats the rule against fresh exposure after signing.
A close of the entire freshly observed residual remains exempt. No quantity or
approval is silently changed. Synthetic tests cover long/short closes, exact
boundaries, rounded limits, old plans and residual growth/shrinkage, including
real Unix-socket signing with a deliberately public unfunded fixture key.

The snapshot also retains the dependency updates already merged in public main:
alloy-primitives 1.7.3, arrow-schema 60.1.0, proptest 1.12.0 and signal-hook
0.4.6, alongside the dependency-security fixes below.

## Verification and release boundary

The implementation baseline passed 428 Rust tests (five opt-in live tests
ignored), 32 tooling tests and all 42 official SDK signature/hash vectors.
The local suite includes 88 offline tests with no skips after the close fix.
Formatting, strict all-feature Clippy, Rust 1.94.1 compatibility, release builds
and dependency-policy checks are part of qualification. These counts describe
software checks; they do not certify live venue behavior or production readiness.

Direct chrono use has been removed. UTC formatting uses time; signing expiry
uses integer milliseconds. Arrow/Parquet retains chrono 0.4.45 under a scoped
maintenance exception expiring **2027-01-31**. Version, parents, direct use,
signing dependencies and expiry are checked in CI. See
[dependency review](docs/dependency-review-2026-11-22.md).

**Mainnet release remains closed.** Freeze the exact clean source, lockfile,
Python environment and release binaries after checks. Complete three independently
reviewed credentialed sessions covering full, partial and fault/restart lifecycle
behavior. A partial close does not prove a partial entry fill or repaired exits.
Retain alert acknowledgement, manual fallback, fresh accounting and independent
custody/implementation review before an exact-plan human release decision.
Real account identities, session artifacts, fills, balances and reviewer records
belong in private storage, not this status page or public test fixtures.

Use [the attended runbook](docs/attended-testnet-session.md),
[validation matrix](docs/local-operator-validation.md) and
[mainnet pilot contract](docs/local-mainnet-bracket-pilot.md).

The FIFO filesystem regression runs in a separate integration-test process.
This prevents its helper process from briefly inheriting another test’s outbox
lock during concurrent reopen checks; production locking remains unchanged.

## Reproducible checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo build --workspace --all-features --release --locked
python3 -m unittest discover -s tools/tests
uv run --offline --no-project --with hyperliquid-python-sdk==0.24.0 python tools/check_local_operator.py
tools/check-rust-supply-chain.sh
cargo deny check
python3 tools/check_dependency_policy.py
python3 tools/check_public_export.py
```

Ordinary tests are offline. Opt-in tests marked `ignored` use live endpoints and
must be selected deliberately after inspecting their effects. They do not supply
credentialed lifecycle evidence merely by passing. Public examples and signing
fixtures use synthetic identities or deliberately public unfunded keys.

## Keeping this file current

Update capabilities, limitations, verification results and this review date when
implementation changes. Preserve the distinction between offline checks, live
technical evidence, independent review and release approval. Keep operator
histories private and inspect the exact filtered diff before public publication.
