# hypercarry

Hyperliquid funding recorder & predictor — a Rust tool that records perpetual
funding, open interest, and order-book data to a local, queryable, backtestable
dataset, and computes funding APR, perp–spot basis, cross-venue funding spreads,
and a predicted next-hour funding rate.

> Not affiliated with Hyperliquid. The default CLI consumes only public,
> read-only market data. An optional default-off library feature can execute on
> testnet through an external signer; there is no built-in key loader or
> mainnet transport. Not trading advice.

## Status

**M9 gate engineering is complete; mainnet release readiness remains closed
pending independently reviewed testnet evidence and a reviewed transport.**
What exists today:

See [DEV_STATUS.md](DEV_STATUS.md) for the continuously maintained capability
matrix, live-integration readiness, design decisions, and recommended next work.

- A Cargo workspace with `hypercarry-core` (domain/client/predictor library),
  `hypercarry-recorder` (bounded WebSocket capture/replay),
  `hypercarry-storage` (versioned dataset contracts), and `hypercarry-cli`
  (binary), plus the optional `hypercarry-execution` simulation/dry-run crate
  and a non-published `hypercarry-testnet-operator` evidence harness.
- Typed models for the three Hyperliquid `info`-endpoint responses the tool
  consumes (`fundingHistory`, `metaAndAssetCtxs`, `predictedFundings`). All
  monetary and rate fields are `rust_decimal::Decimal`, never `f64`. Shapes are
  verified against live API responses, including the `null` price fields that
  delisted assets carry.
- Golden deserialize tests on recorded payloads, so schema drift breaks CI.
- A typed, async, traced client for the three read-only Hyperliquid `info`
  requests. Mainnet/testnet selection is explicit, custom development endpoints
  enforce HTTPS away from loopback, and the injectable transport keeps normal
  tests entirely on recorded fixtures.
- Opt-in live REST smoke tests cover all three requests on mainnet and testnet;
  they remain ignored during normal offline test runs.
- Operational `snapshot`, `backfill`, `record`, `apr`, `basis`, `spread`,
  `predict`, and `tui` commands with explicit network and coin selection.
  Snapshot combines current context, venue predictions, and
  recent settled funding.
  Backfill resumes into the atomic Parquet dataset with progress and Ctrl-C
  cancellation; APR reads the latest settlement and annualizes it. APR's
  default operator view uses UTC timestamps, signed percentages and basis
  points, and a verified contiguous-history duration. Automation-oriented
  commands provide versioned JSON; layered commands resolve flags > environment
  > JSON config. The TUI is intentionally interactive, while `basis` and
  `spread` are deterministic local calculations. Exit code 3 remains reserved
  for backward compatibility.
- A validated settled-funding Parquet v1 schema with deterministic identity,
  safe Hive partition paths, exact decimal handling, and reproducibility
  provenance. Its production writer performs sorted overlap deduplication,
  atomic daily-partition replacement, and monotonic per-stream checkpoints.
  The contract is documented in
  [docs/settled-funding-parquet-v1.md](docs/settled-funding-parquet-v1.md).
- Inclusive-boundary `fundingHistory` pagination with overlap deduplication,
  deterministic ordering, stalled-page protection, bounded jittered retries,
  rate-limit awareness, and offline multi-page tests. Schema v1 also passes a
  real Arrow/Parquet write/read round trip.
- CI gating on the declared Rust 1.93 MSRV, `cargo fmt --check`,
  `cargo clippy -D warnings` (pedantic), tests/doctests, dependency policy, and
  an all-feature release build.
- Exact decimal metrics for perp-spot basis, settlement-interval-normalized
  cross-venue funding spread, and funding-window statistics. Examples run in
  the normal suite, invariants are property-tested, and Criterion benchmarks
  cover the metric hot paths.
- An explicit mainnet/testnet public WebSocket subscriber for asset context and
  L2 books, with capped reconnect backoff, heartbeats, staleness detection, and
  cancellation. Every frame enters versioned raw JSONL before a bounded
  normalization queue; query-oriented Parquet and stable health diagnostics
  remain rebuildable through deterministic local replay. The queue's measured
  policy preserves raw frames while dropping and counting only the newest
  normalized projection under pressure.
- An exact-decimal next-hour funding baseline that reconstructs the documented
  impact-price premium, collapses replay into five-second slots, reports
  partial-hour coverage/confidence, and rejects future-data leakage. Realized
  settlement is ground truth; official predictions remain a separately scored
  benchmark. Walk-forward evaluation reports signed, absolute, and rolling
  errors without adding an unproven statistical model.

M1 through M8 and the M9 gate implementation are complete. One clean
credentialed testnet candidate has been recorded, but it remains uncounted
until an independent human emits its reviewer attestation. Operational release
readiness also requires two more reviewed clean sessions, the documented fault
exercises, transport/configuration/dependency security and rollback review, and
a human approval binding the final bundle. The shipped CLI remains read-only.

## Build

```sh
cargo test --workspace # runs the golden deserialize tests
cargo run -p hypercarry-cli -- --help
cargo run -p hypercarry-cli -- snapshot --network testnet --coin BTC
cargo run -p hypercarry-cli -- snapshot --network testnet --coin BTC --output json
cargo run -p hypercarry-cli -- backfill --network testnet --coin BTC --days 7 --dataset data
cargo run -p hypercarry-cli -- apr --network testnet --coin BTC --dataset data
cargo run -p hypercarry-cli -- record --network testnet --coins BTC,ETH --dataset data --output json
cargo run -p hypercarry-cli -- predict --network testnet --coin BTC --capture <raw.jsonl> --settlement-ms <UTC_HOUR_MS> --as-of-ms <CUTOFF_MS> --dataset data --output json
cargo run -p hypercarry-cli -- basis --coin BTC --perp-mark 101 --spot-mid 100
cargo run -p hypercarry-cli -- spread --coin BTC --venue-a Hyperliquid --rate-a 0.0001 --interval-a-hours 1 --venue-b Venue8h --rate-b 0.0004 --interval-b-hours 8
cargo run -p hypercarry-cli -- tui --network testnet --coin BTC --capture <raw.jsonl> --color auto
cargo run -p hypercarry-cli -- completions zsh > _hypercarry
cargo run -p hypercarry-cli -- manpage > hypercarry.1
```

APR defaults to a copy-friendly operator view; `--output json` preserves the
stable schema-v1 automation contract:

```text
BTC-PERP · HYPERLIQUID TESTNET

Settlement       2026-08-24T20:00:00.017Z
Hourly funding   +0.05443322%  (+5.443322 bps)
Simple APR       +476.8350072%
History          168 hourly observations (7 days)
```

### Configuration

- Common: `HYPERCARRY_NETWORK`, `HYPERCARRY_COIN`, `HYPERCARRY_COINS`,
  `HYPERCARRY_OUTPUT`, and `HYPERCARRY_TRACING`.
- Storage and recording: `HYPERCARRY_DATASET`, `HYPERCARRY_DAYS`, and
  `HYPERCARRY_QUEUE_CAPACITY`.
- Prediction replay: `HYPERCARRY_CAPTURE`, `HYPERCARRY_SETTLEMENT_MS`,
  `HYPERCARRY_AS_OF_MS`, `HYPERCARRY_OFFICIAL_RATE`, and
  `HYPERCARRY_OFFICIAL_OBSERVED_AT_MS`.
- TUI: `HYPERCARRY_REFRESH_MS` and `HYPERCARRY_COLOR`.
- JSON configuration: `--config <PATH>` or `HYPERCARRY_CONFIG`.

Command-line values take precedence over environment variables and JSON
configuration.

### Reference documentation

- Recording and replay: [live-market-recording-v1](docs/live-market-recording-v1.md).
- Prediction, causality, confidence, and evaluation:
  [funding-prediction-v1](docs/funding-prediction-v1.md).
- CLI contracts and recovery: [cli-contracts-v1](docs/cli-contracts-v1.md) and
  [operator-recovery](docs/operator-recovery.md).
- Simulation, dry-run behavior, and journal schema:
  [execution-simulation-v1](docs/execution-simulation-v1.md).
- Risk, signer isolation, durable identity, throttling, and recovery:
  [execution-safety-v1](docs/execution-safety-v1.md).
- Testnet execution and operations: [testnet-execution-v1](docs/testnet-execution-v1.md),
  [hyperliquid-testnet-runbook](docs/hyperliquid-testnet-runbook.md), and
  [testnet-funding](docs/testnet-funding.md).

The optional `hypersdk-signer` feature pins the signing SDK only inside the
execution crate; it is absent from default/read-only builds.
The testnet-only operator, its strict secret-free configuration, owner-only
local signer protocol, non-overwriting evidence boundary, and offline
independent-review attestation are documented in
[docs/testnet-operator-v1.md](docs/testnet-operator-v1.md). The binary does not
custody keys. A clean credentialed place/cancel/reconcile candidate was recorded
on 2026-09-01; it is not yet one of the three required sessions because an
independent human has not emitted its attestation. A separate non-published
Foundry-keystore provider can serve the signer protocol by delegating
interactive prehash signing to `cast`; it never accepts a raw key or password.
Immediately before signing, the operator also verifies the account abstraction
mode, mode-appropriate available collateral, open-order count, and
agent-to-master authorization against the official testnet API.
The first credentialed signing exercise failed closed before exchange
submission on a leading-zero scalar encoding mismatch; `0b665ca` fixes the
width and the next attempt must use entirely fresh one-shot session artifacts.
M9's default-off schema-v2 reviewed-bundle evidence, exact-order authorization,
canary, credential, health, reconciliation, alert, audit, rollback, and dead-man
gate is documented in
[docs/mainnet-release-gate-v1.md](docs/mainnet-release-gate-v1.md).

Requires stable Rust 1.93 or newer (`rust-toolchain.toml` selects the stable
channel).

## License

Dual-licensed under either of [Apache-2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT) at your option.

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and safety
boundaries. Report suspected vulnerabilities privately according to
[SECURITY.md](SECURITY.md).
