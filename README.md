# hypercarry

Hyperliquid funding recorder, predictor, and execution research tool.

Hypercarry helps you investigate whether a carry trade earned enough funding to
cover its costs. A typical positive-funding carry trade buys an asset on the
spot market and shorts the same quantity of its perpetual contract. The price
moves partly offset; the short may receive funding from longs. Funding can
reverse, and fees, basis changes and liquidation risk can turn the result into
a loss.

The Rust tool records reproducible funding history, evaluates recorded trades
with explicit valuation assumptions, and estimates the next hour's rate. It
exists to make those inputs and calculations inspectable instead of relying on
an attractive headline APR. It does not choose trades or manage a spot hedge.

```text
BTC-PERP · HYPERLIQUID MAINNET · SHORT CARRY

Window           2026-09-16T20:00:00.000Z → 2026-09-17T20:00:00.000Z (24h)
Settlements      24 applied
Applied range    2026-09-16T20:00:00.091Z → 2026-09-17T19:00:00.007Z
Size             0.5 BTC  (notional 40750 USDC at entry, spot hedged)
Valuation        perp-entry-price at 81500

Funding          +9.909963975 USDC
Perp price       +125 USDC
Spot hedge       -50 USDC
Fees             -73.18125 USDC
                 ─────────────
Net              +11.728713975 USDC  (+0.028782% on notional, +10.505474% APR)
```

Every monetary and rate value is `rust_decimal::Decimal`, never `f64`. The
example uses entry-price funding valuation and supplied fees; it is an
approximation, not an account cash-funding statement.

## Quick start

```sh
cargo run -p hypercarry-cli -- snapshot --network mainnet --coin BTC
```

That is read-only public market data and needs no credentials. To build a local
history and annualize the latest settlement:

```sh
cargo run -p hypercarry-cli -- backfill --network mainnet --coin BTC --days 7 --dataset data
cargo run -p hypercarry-cli -- apr --network mainnet --coin BTC --dataset data
```

```text
BTC-PERP · HYPERLIQUID TESTNET

Settlement       2026-08-24T20:00:00.017Z
Hourly funding   +0.05443322%  (+5.443322 bps)
Simple APR       +476.8350072%
History          168 hourly observations (7 days)
```

The APR output below is an illustrative **testnet** result, not the output of
the mainnet commands above or an expected return.

## Safety model

**The shipped CLI is read-only.** It accepts no private key, signs nothing,
touches no wallet, and places no orders. There is no built-in key loader
anywhere in the workspace.

Execution exists, is default-off, and lives in separate crates. No mainnet
order has been submitted by this implementation. Production custody integration,
parts of the supervisor and independent release evidence remain unfinished.
The release gate refuses to authorize an order until its requirements hold:

- Authorization requires three independently reviewed credentialed testnet
  sessions, a frozen bundle identifying the exact commit, lockfile digest and
  binary hash, and a human decision whose SHA-256 digest binds all of it, so
  none of it can be edited after approval.
- Before every order it rechecks reconciliation state, unmanaged-order and
  unresolved-submission counts, REST and private-stream latency, the audit
  journal, the rollback path, and a process-independent dead-man switch.
- The authorization capability has no public constructor, carries one exact
  order, is consumed by value, and expires on a short reviewed TTL.
- The kill switch is rechecked at the final irreversible submit boundary,
  after the signer round trip.
- The binary embeds its source commit and lockfile digest; one built from a
  dirty tree cannot pass preflight.

The default-off `hypercarry-mainnet-services` crate supplies the signer service
and watchdog, including bounded reduce-only recovery. Key custody remains in an
external provider; hosts, provider integration and alert delivery still require
provisioning. See [operational services](docs/mainnet-services-v1.md). Health supervision is partly shipped, as the
credential-free `hypercarry-supervisor`; what it has not yet measured it reports
as not ready, so the executor declines.

**Current release decision: closed.** See
[mainnet-release-gate-v1](docs/mainnet-release-gate-v1.md) for what remains.

Not affiliated with Hyperliquid. Not trading advice.

## Commands

| Command | Purpose | External I/O |
|---|---|---|
| `snapshot` | Current context, venue predictions, recent settlements | Read-only HTTPS |
| `backfill` | Resumable settled-funding history into Parquet | Read-only HTTPS, local writes |
| `record` | Public asset-context and L2 book capture | Read-only WebSocket, local writes |
| `apr` | Latest realized funding, annualized | Local Parquet |
| `pnl` | What one recorded carry position earned | Local Parquet, trade document |
| `basis` | Exact perp-spot basis | None |
| `spread` | Interval-normalized cross-venue funding spread | None |
| `predict` | Causal next-hour funding estimate from a capture | Local JSONL, optional Parquet |
| `tui` | Live view over an actively appended capture | Local JSONL |

```sh
cargo run -p hypercarry-cli -- --help
cargo run -p hypercarry-cli -- record --network mainnet --coins BTC,ETH --dataset data --output json
cargo run -p hypercarry-cli -- pnl --network mainnet --coin BTC --dataset data --trade <trade.json>
cargo run -p hypercarry-cli -- basis --coin BTC --perp-mark 101 --spot-mid 100
cargo run -p hypercarry-cli -- spread --coin BTC \
  --venue-a Hyperliquid --rate-a 0.0001 --interval-a-hours 1 \
  --venue-b Venue8h --rate-b 0.0004 --interval-b-hours 8
cargo run -p hypercarry-cli -- predict --network mainnet --coin BTC \
  --capture <raw.jsonl> --settlement-ms <UTC_HOUR_MS> --as-of-ms <CUTOFF_MS> --dataset data
cargo run -p hypercarry-cli -- tui --network mainnet --coin BTC --capture <raw.jsonl> --color auto
cargo run -p hypercarry-cli -- completions zsh > _hypercarry
cargo run -p hypercarry-cli -- manpage > hypercarry.1
```

Human views are copy-friendly; `--output json` preserves a stable schema-v1
automation contract with decimals as base-10 strings. Diagnostics, progress and
tracing go to stderr. Exit codes are stable and documented in
[cli-contracts-v1](docs/cli-contracts-v1.md).

## How it is built

- **Exact arithmetic.** No monetary or rate value passes through `f64`.
  Fund-affecting operations are checked, so out-of-range inputs return errors
  rather than panicking or wrapping.
- **Reproducible data.** A versioned settled-funding Parquet schema with
  deterministic identity, safe Hive partition paths, sorted overlap
  deduplication, atomic daily-partition replacement, monotonic checkpoints and
  ingestion provenance. Raw WebSocket frames land in versioned JSONL before any
  normalization, so Parquet and health diagnostics are rebuildable by replay.
- **Causal prediction.** The next-hour baseline reconstructs the documented
  impact-price premium, reports partial-hour coverage and confidence, and
  rejects future-data leakage. Realized settlement is ground truth; official
  predictions are scored as a separate benchmark.
- **Offline tests.** Normal runs use recorded fixtures; live REST and WebSocket
  smoke tests are opt-in and ignored by default. Golden deserialize tests break
  CI on upstream schema drift.
- **CI gates** the declared Rust 1.93 MSRV, `cargo fmt --check`, pedantic
  `cargo clippy -D warnings`, tests and doctests, `cargo deny`, and an
  all-feature release build.

Workspace crates: `hypercarry-core` (domain, read-only client, metrics, carry
ledger, predictor), `hypercarry-storage` (dataset contracts),
`hypercarry-recorder` (bounded capture and replay), `hypercarry-cli` (the
read-only binary), `hypercarry-execution` (venue-neutral risk, lifecycle,
journal, release gate), `hypercarry-hyperliquid` (venue adapter and signing
boundary), `hypercarry-executor` (default-off manual or one-use-approved canary),
`hypercarry-mainnet-services` (isolated signer and emergency watchdog),
`hypercarry-mcp` (optional scoped stdio interface),
`hypercarry-mainnet-config` (runtime configuration, its reviewed digest, and the
health-file contract), `hypercarry-supervisor` (default-off, credential-free
account-health observer), and a non-published `hypercarry-testnet-operator`
evidence harness.

## MCP integration

The optional `hypercarry-mcp` stdio server exposes analytics to MCP clients.
It defaults to read-only; explicit `--scope read,write` permits local backfills
and saved trade records. The separate `trade` scope plus `--enable-mainnet`
can consume a short-lived, single-use operator approval for the exact reviewed
canary; it cannot change the order or bypass the release gate. Responses retain
decimal strings, network, observation time, units, data coverage, valuation
assumptions and provenance. Prediction confidence measures data completeness,
not probability of profit. See [MCP integration](docs/mcp-v1.md).

## Configuration

Layered commands resolve command-line flags, then `HYPERCARRY_*` environment
variables, then the named JSON configuration section. Network and coin never
silently default.

- Common: `HYPERCARRY_NETWORK`, `HYPERCARRY_COIN`, `HYPERCARRY_COINS`,
  `HYPERCARRY_OUTPUT`, `HYPERCARRY_TRACING`.
- Storage and recording: `HYPERCARRY_DATASET`, `HYPERCARRY_DAYS`,
  `HYPERCARRY_QUEUE_CAPACITY`.
- Carry P&L: `HYPERCARRY_TRADE`.
- Prediction replay: `HYPERCARRY_CAPTURE`, `HYPERCARRY_SETTLEMENT_MS`,
  `HYPERCARRY_AS_OF_MS`, `HYPERCARRY_OFFICIAL_RATE`,
  `HYPERCARRY_OFFICIAL_OBSERVED_AT_MS`.
- TUI: `HYPERCARRY_REFRESH_MS`, `HYPERCARRY_COLOR`.
- JSON configuration: `--config <PATH>` or `HYPERCARRY_CONFIG`.

## Status

M1 through M8 and the M9 release-gate implementation are complete. The mainnet
adapter and manual executor are implemented behind default-off features;
mainnet release readiness is **closed**.

One clean credentialed testnet place/cancel/reconcile candidate was recorded on
2026-09-01. It does not yet count as one of the three required sessions because
no independent human has emitted its reviewer attestation.

[DEV_STATUS.md](DEV_STATUS.md) is the continuously maintained capability matrix,
live-integration readiness record, and design-decision log.

## Reference documentation

**Data and analytics**

- [settled-funding-parquet-v1](docs/settled-funding-parquet-v1.md) — dataset contract
- [live-market-recording-v1](docs/live-market-recording-v1.md) — capture and replay
- [funding-prediction-v1](docs/funding-prediction-v1.md) — causality, confidence, evaluation
- [cli-contracts-v1](docs/cli-contracts-v1.md) — output contracts and exit codes
- [operator-recovery](docs/operator-recovery.md) — recovery procedures

**Execution (default-off)**

- [mainnet-release-gate-v1](docs/mainnet-release-gate-v1.md) — the authorization gate
- [mainnet-integration-v1](docs/mainnet-integration-v1.md) — adapter, executor, required external services
- [execution-safety-v1](docs/execution-safety-v1.md) — risk, signer isolation, durable identity
- [execution-simulation-v1](docs/execution-simulation-v1.md) — dry-run behavior and journal schema
- [testnet-execution-v1](docs/testnet-execution-v1.md), [hyperliquid-testnet-runbook](docs/hyperliquid-testnet-runbook.md) and [testnet-funding](docs/testnet-funding.md) — testnet operation
- [testnet-operator-v1](docs/testnet-operator-v1.md) — evidence harness, owner-only signer protocol, reviewer attestation
- [testnet-execution-evidence](docs/testnet-execution-evidence.md) — recorded session evidence
- [adversarial-security-review-v1](docs/adversarial-security-review-v1.md) — findings and accepted backlog

The optional `hypersdk-signer` feature pins the signing SDK inside
`hypercarry-hyperliquid` and is absent from the read-only CLI. The testnet
operator does not custody keys; a separate non-published Foundry-keystore
provider can serve the signer protocol by delegating interactive prehash
signing to `cast`, and never accepts a raw key or password.

## Requirements

Stable Rust 1.93 or newer; `rust-toolchain.toml` selects the stable channel.

## License

Dual-licensed under either of [Apache-2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT) at your option.

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and safety
boundaries. Report suspected vulnerabilities privately according to
[SECURITY.md](SECURITY.md).
