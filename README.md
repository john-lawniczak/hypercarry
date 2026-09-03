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

## Roadmap

| Milestone | Delivers |
|-----------|----------|
| M0 | Workspace, response types, golden tests, CI. *(done)* |
| M1 | `info` client + resumable Parquet `backfill` + dataset-driven `apr`. *(done)* |
| M2 | Basis, cross-venue spread, and funding-window metrics with property tests and Criterion benches. *(done)* |
| M3 | Live WebSocket recorder (reconnect + backpressure); deterministic replay tests. *(done)* |
| M4 | Next-hour funding predictor, validated against realized settlement. *(done)* |
| M5 | Complete CLI contracts, recovery behavior, artifacts, and live replay TUI. *(done)* |
| M6 | Optional isolated execution model, deterministic simulator, and non-signing dry-run. *(done)* |
| M7 | Risk controls, signing isolation, idempotency, reconciliation, and recovery. *(done)* |
| M8 | Feature-gated Hyperliquid testnet execution lifecycle. *(implemented; one clean credentialed candidate awaits independent review)* |
| M9 | Explicit, evidence-backed mainnet release gate. *(engineering complete; operational release closed)* |

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

Command environment variables are `HYPERCARRY_NETWORK`, `HYPERCARRY_COIN`,
`HYPERCARRY_COINS`,
`HYPERCARRY_OUTPUT`, `HYPERCARRY_TRACING`, `HYPERCARRY_DATASET`, and
`HYPERCARRY_DAYS`; recorder queue size is `HYPERCARRY_QUEUE_CAPACITY`. Predictor
replay also supports `HYPERCARRY_CAPTURE`, `HYPERCARRY_SETTLEMENT_MS`,
`HYPERCARRY_AS_OF_MS`, `HYPERCARRY_OFFICIAL_RATE`, and
`HYPERCARRY_OFFICIAL_OBSERVED_AT_MS`. The TUI also supports
`HYPERCARRY_REFRESH_MS` and `HYPERCARRY_COLOR`. Use
`--config <PATH>` (or `HYPERCARRY_CONFIG`) for a JSON file; command-line values
take precedence. The recording contracts are documented in
[docs/live-market-recording-v1.md](docs/live-market-recording-v1.md).
The M4 formula, causality, confidence, benchmark, and walk-forward contracts are
documented in [docs/funding-prediction-v1.md](docs/funding-prediction-v1.md).
All command/output conventions and recovery procedures are collected in
[docs/cli-contracts-v1.md](docs/cli-contracts-v1.md) and
[docs/operator-recovery.md](docs/operator-recovery.md).
The optional execution types, deterministic fill assumptions, dry-run boundary,
and journal schema are documented in
[docs/execution-simulation-v1.md](docs/execution-simulation-v1.md).
M7 risk, signer isolation, durable identity, throttling, and recovery contracts
are documented in [docs/execution-safety-v1.md](docs/execution-safety-v1.md).
M8's testnet-only adapter and operator procedures are documented in
[docs/testnet-execution-v1.md](docs/testnet-execution-v1.md) and
[docs/hyperliquid-testnet-runbook.md](docs/hyperliquid-testnet-runbook.md).
Wallet roles, the mainnet deposit prerequisite, mock USDC drip, and optional
HyperEVM gas funding are covered in the user-facing
[docs/testnet-funding.md](docs/testnet-funding.md).
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
