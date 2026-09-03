# Hypercarry development status

Last reviewed: **2026-09-03**

This is the living engineering-status document for hypercarry. Update it when a
capability lands, a milestone changes state, or a verification command changes.
`README.md` describes the product, while `TODO.md` remains the detailed backlog.

## Product goal

Hypercarry is a local-first Hyperliquid funding research application. It is
intended to collect public perpetual-market data, retain a reproducible history,
calculate carry metrics, evaluate a next-hour funding estimate, and eventually
present the results through a polished command-line and terminal interface.

The default CLI is deliberately read-only. It does not accept private keys,
sign requests, access a wallet, or place orders. The optional execution library
can place testnet orders only through a caller-supplied external signer; it has
no built-in key loader and no mainnet transport.

## Status at a glance

**Current phase: M9 gate engineering complete; mainnet release readiness is
closed pending independently reviewed repeated testnet evidence and a reviewed
transport.**

| Capability | State | Notes |
|---|---|---|
| Rust workspace and CI gates | Complete | Formatting, strict Clippy, tests/doctests, MSRV check, and release build |
| Hyperliquid response models | Complete | Funding history, perp metadata/contexts, predicted funding |
| Recorded-payload schema tests | Complete | Includes nullable data from delisted assets |
| Typed HTTPS `info` client | Complete | Async reqwest + Rustls, bounded I/O, typed HTTP/rate-limit errors, tracing |
| Explicit network identity | Complete | Typed mainnet/testnet endpoints; guarded custom development endpoints |
| Injectable fixture transport | Complete | Normal tests do not access the network |
| Opt-in live REST smoke tests | Complete | Ignored by default; covers both networks and all three typed requests |
| Historical backfill | Complete | Resumable CLI, inclusive pagination, bounded retries, progress, cancellation, and atomic checkpoints |
| Parquet dataset | Complete | Production Arrow/Parquet read/write, deterministic deduplication, atomic replacement, and schema v1 provenance |
| Metrics | Complete | Exact basis, interval-normalized spread, window stats, property tests, and Criterion benchmarks |
| End-user CLI commands | Complete | All read-only roadmap commands are operational; stable JSON, completions, and man generation are documented |
| Live WebSocket recorder | Complete | Explicit network, reconnect/heartbeat/staleness, bounded normalization, versioned raw capture/Parquet, replay |
| Funding predictor | Complete | Exact documented baseline, raw replay, causal evaluation, official benchmark, and walk-forward errors |
| TUI | Complete | Incremental raw-capture replay, bounded refresh, explicit UTC/units, accessible color modes, and safe terminal restoration |
| Execution simulation | Complete | Optional venue-neutral crate; exact validation, deterministic fills, non-signing dry-run, and schema-v1 journal |
| Execution safety | Complete | Comprehensive exact risk policy, filesystem kill switch, isolated network signer, durable client IDs, bounded retry/throttle, locked journal, and lifecycle recovery |
| Testnet execution | Feature-gated, live candidate unreviewed | Official fixed HTTPS/WebSocket endpoints, pinned SDK signer, place/cancel, private events, REST reconciliation, restart recovery, non-shipping external-signer harness, and offline reviewer attestation; one clean credentialed candidate awaits independent human review |
| Mainnet release gate | Implemented, closed | Default-off compile gate plus evidence digest, separate credentials, explicit/manual enablement, canary, continuous health/reconciliation, alerts/audit, rollback, and dead-man checks; required evidence is incomplete |
| Mainnet transport | Absent | Deliberately deferred until credentialed testnet passes; its exact reviewed revision must then be bound into release evidence |

## What works now

- `hypercarry-core` models the three public Hyperliquid `info` responses used by
  the roadmap: `fundingHistory`, `metaAndAssetCtxs`, and `predictedFundings`.
- `hypercarry-core` warns on missing public API documentation, and representative
  exact-decimal metric examples run as part of the doctest suite.
- Prices and rates use `rust_decimal::Decimal`, avoiding binary floating-point
  drift in financial calculations.
- Basis returns absolute and ratio values and rejects a non-positive spot with
  a typed error. Cross-venue spread normalizes each settlement rate to hourly,
  and window aggregation reports mean/min/max, strict sign flips, and realized
  APR. Property tests cover sign, scale, extrema, and normalized equivalence.
- Metadata and live asset contexts can be joined safely by index with an
  explicit length-mismatch error.
- Each typed request determines its response type at compile time.
- `Network::{Mainnet, Testnet}` owns each official `info` endpoint and provides
  stable lowercase names for configuration, tracing, and future stored output.
- `ReqwestInfoTransport` is asynchronous, requires an explicit network, uses
  Rustls, and bounds both connection setup and the complete request.
- HTTP failures retain endpoint, status, timeout classification, and a standard
  `Retry-After` value when present. Response bodies are neither retained in
  errors nor emitted through tracing.
- Custom endpoints are available only through the explicitly named development
  constructor. Non-loopback endpoints must use HTTPS; loopback HTTP remains
  available for local integration tests.
- Fixture-backed tests validate request JSON and response decoding without
  depending on network availability or mutable market values.
- `snapshot --network <mainnet|testnet> --coin <COIN>` fetches the selected
  asset context, venue predictions, and a bounded four-hour funding window. Its
  human output includes network identity, normalized decimals, and explicit
  millisecond timestamp labels.
- Snapshot configuration resolves once as command-line flags > `HYPERCARRY_*`
  environment variables > an explicit JSON config file. Network and coin have
  no silent defaults.
- `--output json` emits schema version 1; `--tracing` selects quiet, normal,
  verbose, or diagnostic metadata-only logging on stderr.
- `backfill` resolves an explicit network and coin, inclusive day window, and
  dataset root through flags > environment > JSON config. It resumes from the
  durable checkpoint, reports fetch/commit progress on stderr, and Ctrl-C
  cancels an in-flight fetch before storage mutation.
- `apr` scans the selected Parquet stream, chooses its latest deterministic
  settlement, and annualizes the hourly rate. Its operator view uses ISO-8601
  UTC, exact signed percentages and basis points, prominent network identity,
  and a day count only for contiguous hourly observations; JSON schema v1 is
  unchanged. Empty streams produce an actionable partial-data error.
- `hypercarry-storage` defines and validates the settled-funding Parquet v1
  schema, deterministic identity `(network, venue, coin, settlement_time_ms)`,
  safe Hive partition path, exact decimal scale, and row-level provenance.
- `InfoClient::funding_history_range` follows Hyperliquid's 500-row inclusive
  pagination boundary, removes an identical overlap, preserves sorted output,
  and rejects conflicting, out-of-window, wrong-coin, or stalled pages.
- Funding-history pages retry transient connection/body failures, HTTP 408/429,
  and HTTP 5xx with four bounded attempts, capped exponential full jitter, and
  safe delta-seconds `Retry-After` handling. Encoding, schema, and ordinary 4xx
  failures are never retried.
- The production dataset writer merges daily partitions in identity order,
  collapses equivalent overlaps with deterministic provenance, and rejects
  conflicting rates without replacing the existing file.
- Parquet partitions and per-stream JSON checkpoints use same-directory atomic
  replacement with file synchronization. Checkpoints advance only after data
  commits and never regress, so inclusive replay after interruption is safe.
- Real Arrow/Parquet interoperability tests check every identity, decimal,
  timestamp, and provenance column through a write/read cycle.
- `record --network <mainnet|testnet> --coins <COIN,...>` subscribes directly to
  Hyperliquid public `activeAssetCtx` and `l2Book` feeds without an exchange
  SDK. It applies a capped reconnect backoff, application heartbeats, stale
  connection recycling, and a clean Ctrl-C close boundary.
- Every WebSocket text frame is appended with network, session, connection,
  receive sequence, and receive timestamp before normalization. Disconnect and
  stale boundaries are captured alongside frames in raw JSONL schema v1.
- Normalization uses a bounded queue and `drop_newest_normalized` policy. Raw
  data is preserved under pressure while exact drops are reported. Accepted
  asset context and top-of-book values are written incrementally to exact
  Decimal128 Parquet schema v1; the full canonical JSON payload remains in each
  row for future projections.
- `ReplayTransport` yields raw frames and connection boundaries in immutable
  file order. Offline tests cover reconnects, clean cancellation, exact
  duplicates, distinct/out-of-order timestamps, stale source and transport
  data, malformed frames, additive v1 fields, and rejected future schemas.
- Recorder diagnostics schema v1 exposes paths, connection/reconnect/heartbeat
  counts, queue policy, raw/normalized/drop counts, and parse/order/stale health
  as stable human or JSON output. See
  `docs/live-market-recording-v1.md` for the frozen contracts.

`snapshot`, `backfill`, `record`, `apr`, `basis`, `spread`, `predict`, and `tui`
are operational. Shell completion and roff man-page source are generated from
the same Clap command model. CLI schema and recovery behavior are documented in
`docs/cli-contracts-v1.md` and `docs/operator-recovery.md`.

The optional `hypercarry-execution` crate resolves inert intents through exact
venue metadata, conservative price/size quantization, and a fail-closed policy
trait. Its deterministic replay adapter models latency, fees, adverse slippage,
partial fills, cancellation races, and no-fill outcomes. The dry-run adapter
cannot sign or submit and durably journals the exact allowed action in
secret-free JSONL schema v1. See `docs/execution-simulation-v1.md`.

M7 adds a deterministic, explainable policy for all roadmap limits; exact
128-bit client order identities; typed per-network signer-provider validation;
proactive throttling and reconciliation-first retry classification; a
process-independent filesystem kill switch and exclusive journal writer lock;
and strict lifecycle replay that rejects regressive state, timestamps, venue
IDs, or cumulative fills. See `docs/execution-safety-v1.md`. The kill switch
is re-checked a second time immediately before the irreversible network
submit, closing the gap across a potentially interactive signing round-trip;
the client order ID is derived from the validated, venue-quantized order
rather than the pre-quantization intent. See
`docs/adversarial-security-review-v1.md` for the full arithmetic and
race-condition review that produced these and other fixes.

M8 adds a `testnet-execution` Cargo feature with an endpoint-fixed Hyperliquid
adapter. It requests signatures through the isolated provider, places GTC
limits with durable `cloid` and bounded expiry, cancels by `cloid`, reconciles
uncertain requests, and consumes a separate private order/fill WebSocket stream
with durable event deduplication. The default build and shipped CLI remain
read-only. See `docs/testnet-execution-v1.md` and
`docs/hyperliquid-testnet-runbook.md`.

The nested `hypersdk-signer` feature pins `hypersdk` 0.2.15 and provides a
Hyperliquid-aware signer adapter. Deterministic tests prove exact order/cancel
request preservation and recover the selected signing address. One live
credentialed testnet candidate has now completed its bounded lifecycle.

The non-published `hypercarry-testnet-operator` binary assembles one bounded
credentialed testnet place/cancel/REST-reconciliation lifecycle without adding
execution to the default CLI. It talks to a separately managed signer over an
owner-only local Unix socket, rejects unknown config fields and stale snapshots,
rechecks the filesystem kill switch, keeps the journal exclusively locked, and
writes atomic non-overwriting evidence after an independent account-wide
open-orders query. It cannot create or custody the required API wallet, and the
harness itself is not live evidence. See `docs/testnet-operator-v1.md`.

For local evidence collection, a separate non-published Foundry-keystore
provider can serve the owner-only signer protocol. It constrains one order and
its matching cancel, delegates interactive prehash signing to an absolute
`cast` executable, verifies the recovered agent address, and never accepts a
raw key or password. Its presence does not count as credentialed testnet
evidence.

The operator's live pre-signing gate is account-mode aware. It verifies the
official `userAbstraction` response, sources unified-account available USDC
from spot state or standard-account equity from perpetual state, requires the
reviewed open-order count to remain exact, and confirms the agent still belongs
to the configured master account. The first clean credentialed candidate is
recorded below and remains outside the release count pending independent review.

On 2026-08-31, a credentialed Foundry signer produced the first bounded order
signature, but the operator rejected it before `/exchange`: HyperSDK serialized
a leading-zero signature scalar at minimal width while the execution boundary
requires exact 32-byte `r` and `s` fields. An immediate independent account
query returned zero open orders. Commit `0b665ca` now emits fixed-width scalars
and has regression coverage. This failed-closed signing exercise is not a
completed testnet session; the next attempt must use a fresh one-shot signer,
config timestamps, session/correlation IDs, journal, and evidence path.

The Foundry signer now handles Ctrl-C and termination signals while waiting for
an order or cancel connection, exits without advancing the one-shot phase, and
removes its Unix socket through the existing cleanup guard. This makes the
required failed-attempt teardown deterministic; a live operator must still
confirm the old process and socket are gone before starting fresh artifacts.

On 2026-09-01, a fresh `df1f911` session passed its offline and live gates but
timed out at the isolated signer boundary while the interactive `cast` child
remained active. The durable journal stopped after risk approval and the exact
action, an independent official query returned zero open orders, and no evidence
file was created. The exact child and signer were stopped and the socket was
removed. The signer now also cancels and reaps an in-flight `cast` child on
shutdown; another attempt requires a new reviewed commit, signer, snapshot,
session/correlation IDs, journal, config, and evidence path.

A subsequent fresh `e11e7ff` session signed and submitted the bounded order and
its exact matching cancel. Hyperliquid assigned venue order `59070902058`, but
the operator stopped during reconciliation because the official `orderStatus`
response uses a `{"status":"order","order":{"order":...,"status":...}}`
envelope rather than the flattened shape in the parser. Independent queries by
venue ID and `cloid` both returned `canceled`; account-wide open orders were
zero and matching fills were empty. No lifecycle evidence was produced, so the
session does not count toward readiness. The parser now accepts the current
nested response while retaining its fixture-covered legacy shape.

Commit `34644b0` then completed the first clean credentialed candidate lifecycle
from 2026-09-01T16:03:57Z through 16:04:23Z. The at-most-$12 BTC order used
client ID `0x6f09df9c73c077a5f3c31f53fc906176`; venue order `59071289661`
reconciled from open through cancel-pending to cancelled with zero fill,
unresolved submissions, policy bypasses, and harness-reported open orders. A
separate official check returned `canceled`, zero account-wide open orders, and
zero matching fills. Config, journal, evidence, executable, and independent
check hashes are recorded in `docs/testnet-execution-evidence.md`; an automated
secret-pattern scan was clear and the artifacts are owner-only. This remains
candidate evidence—not session one of three—because its immutable harness record
correctly leaves the independent reviewer and manual secret-scan fields unset.

The operator now also exposes a non-networking `review` workflow for a separate
human reviewer. It accepts only absolute owner-only artifacts, strict unreviewed
session evidence, and an exact acknowledgement; verifies the evidence/config/
journal hashes against the artifact manifest; replays the one-order testnet
journal; and cross-checks the official terminal order, account-wide open-order,
and user-fill files. It then atomically writes a separate, owner-only,
non-overwriting schema-v1 attestation that binds the session interval, reviewer,
clean release-gate counts, executable and artifact hashes, and final-state check.
It never edits the harness evidence and cannot itself establish that the human
reviewer is organizationally independent. The current candidate has not yet
been attested.

M9 adds a separate default-off `mainnet-execution` release-gate module, but no
mainnet transport. Schema-v2 evidence cryptographically binds repeated clean
sessions and their independent checks to the exact code/lock/config/transport,
signer alias, dependency/security reviews, rollback record, and later human
decision. It enforces explicit/manual enablement, one-market canary limits,
fresh continuous health/reconciliation, alerts, synchronized audit state, and
a filesystem dead-man heartbeat. Each short-lived authorization contains one
exact order and is consumed to retrieve it. The checked-in evidence example is
intentionally incomplete and cannot construct the gate. Evidence authenticity
remains an external review responsibility. See `docs/mainnet-release-gate-v1.md`.

## Terminology and intended behavior

### Parquet dataset

Apache Parquet is a compressed, column-oriented file format for analytical
data. Unlike a line-oriented JSON or CSV log, a query can read only the columns
it needs, such as `coin`, `time`, and `funding_rate`. This makes long histories
smaller and faster to scan from tools such as DuckDB, Polars, Arrow, and data
science notebooks.

For hypercarry, the dataset becomes the durable boundary between data collection
and analysis. Settled-funding schema v1 is partitioned by schema version,
network, venue, coin, and UTC settlement date. Its deterministic identity is
`(network, venue, coin, settlement_time_ms)`. Every row carries endpoint class,
ingestion time, inclusive request bounds, and software/schema versions. See
`docs/settled-funding-parquet-v1.md` for the frozen contract.

### APR

APR means annual percentage rate. It answers: “If this funding rate continued
for a year without compounding, what annual rate would it imply?”

Hyperliquid settles funding hourly, so a settled hourly rate is annualized as:

```text
APR = hourly funding rate × 24 × 365
    = hourly funding rate × 8,760
```

For example, `0.0001` per hour is `0.876`, or **87.6% APR**. This is a
standardized comparison, not a promise that the rate will persist and not a
compound annual growth rate.

### Next-hour funding prediction

Hyperliquid's funding rate is driven by a premium index derived from impact bid
and ask prices relative to the oracle. Premium samples are taken every five
seconds throughout the hour and averaged. The documented formula is expressed
as an eight-hour rate and then paid hourly at one eighth of that result:

```text
F_8h = average_premium + clamp(interest_rate - average_premium, -0.0005, 0.0005)
F_hour = F_8h / 8
```

The M4 predictor is deterministic and explainable:

1. Record timestamped premium samples and the corresponding oracle and impact
   prices throughout each funding hour.
2. Reproduce the documented time-window aggregation and funding formula.
3. Estimate the unfinished portion of the current hour from observations seen
   so far, producing a predicted settlement rate and a confidence/coverage
   measure.
4. After settlement, join the prediction to the realized funding rate and store
   absolute error, signed error, and rolling error statistics.
5. Use the official `predictedFundings` response as a comparison signal or
   benchmark, not as ground truth for the project's own predictor.

The implementation combines the latest recorded asset-context oracle with full
L2 depth, reconstructs the documented impact execution prices and premium,
collapses updates into five-second slots, attaches coverage and a conservative
completeness score, and excludes samples after each historical cutoff. It joins
only to the same realized UTC settlement hour and reports signed, absolute, and
trailing errors. Official predictions carry their own observation time and are
scored separately as a benchmark. See `docs/funding-prediction-v1.md` for the
frozen schema-v1 behavior.

A statistical or machine-learning model remains deliberately absent. Any
future learned model must be time-split and demonstrate an out-of-sample
improvement over this baseline.

Official references:

- [Hyperliquid funding](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/funding)
- [Hyperliquid info endpoint](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint)
- [Hyperliquid perpetual info requests](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint/perpetuals)

## HTTPS and transport security

The production client already uses HTTPS. “HTTP client” is the generic name for
software that speaks the HTTP protocol family; the configured endpoint begins
with `https://`, and reqwest uses the explicitly enabled Rustls backend for TLS
certificate verification and encrypted transport.

Tests replace the network transport with an in-memory fixture transport. That
is intentional: deterministic unit and schema tests should not become flaky
when the exchange, internet, or live prices change.

## Live integration readiness

The public, read-only REST smoke harness covers mainnet and testnet. It fetches
metadata/contexts, predicted funding, and a bounded four-hour BTC funding window
while checking stable semantic invariants rather than mutable values or exact
asset counts. The tests are ignored by default so offline CI remains reliable:

```sh
cargo test -p hypercarry-core --test live_info --locked -- --ignored
```

Failures identify the network, endpoint, and request type. The production error
retains HTTP status, timeout classification, and `Retry-After` when available,
while schema errors remain distinct decode failures.

Historical funding responses are capped, so `backfill` paginates from the last
returned timestamp, deduplicates the inclusive boundary, and resumes safely
after interruption. The same command can target testnet for an end-to-end live
storage check without credentials.

The opt-in testnet WebSocket smoke test records BTC public context/book traffic
for ten seconds and validates that both raw and normalized layers receive data:

```sh
cargo test -p hypercarry-recorder --test live_recorder --locked -- --ignored
```

Normal tests use scripted transports and local capture files, so reconnect,
backpressure, staleness, and replay checks do not depend on exchange uptime.

## World-class CLI target

The current CLI has eight operational read-only commands. It should continue to
meet these standards:

- clear discoverable help, examples, shell completions, and consistent naming;
- configuration precedence documented as flags > environment > config file >
  safe defaults;
- human-readable tables by default plus stable `--output json` for automation;
- normalized decimal display with explicit units and timestamps;
- progress reporting for long backfills and clean behavior when interrupted;
- actionable error messages with stable, documented exit codes;
- structured tracing with quiet, normal, verbose, and diagnostic modes;
- deterministic output options suitable for scripts and snapshot tests;
- no secrets in logs, errors, shell history recommendations, or telemetry;
- dry-run and explicit confirmation boundaries if execution is ever added.

### CLI configuration and exit contract

Snapshot, backfill, and APR accept `--network`, `--coin`, `--output`, and
`--tracing`; backfill also accepts `--days`, and the dataset commands accept
`--dataset`. Record accepts `--network`, comma-delimited `--coins`, `--dataset`,
`--queue-capacity`, `--output`, and `--tracing`. Equivalent environment
variables are `HYPERCARRY_NETWORK`, `HYPERCARRY_COIN`, `HYPERCARRY_COINS`,
`HYPERCARRY_OUTPUT`, `HYPERCARRY_TRACING`, `HYPERCARRY_DAYS`,
`HYPERCARRY_DATASET`, and `HYPERCARRY_QUEUE_CAPACITY`. Predict additionally
accepts the raw capture, settlement/cutoff milliseconds, and an optional paired
official rate/observation time through the equivalent `HYPERCARRY_CAPTURE`,
`HYPERCARRY_SETTLEMENT_MS`, `HYPERCARRY_AS_OF_MS`,
`HYPERCARRY_OFFICIAL_RATE`, and `HYPERCARRY_OFFICIAL_OBSERVED_AT_MS` variables.
`--config <PATH>` selects a JSON file; `HYPERCARRY_CONFIG` supplies that path
when the flag is absent:

```json
{
  "snapshot": {
    "network": "testnet",
    "coin": "BTC",
    "output": "json",
    "tracing": "normal"
  },
  "backfill": {
    "network": "testnet",
    "coin": "BTC",
    "days": 30,
    "dataset": "data",
    "output": "json",
    "tracing": "normal"
  },
  "record": {
    "network": "testnet",
    "coins": ["BTC", "ETH"],
    "dataset": "data",
    "queue_capacity": 1024,
    "output": "json",
    "tracing": "normal"
  },
  "apr": {
    "network": "testnet",
    "coin": "BTC",
    "dataset": "data",
    "output": "human",
    "tracing": "normal"
  },
  "predict": {
    "network": "testnet",
    "coin": "BTC",
    "capture": "data/raw/schema_version=1/network=testnet/session.jsonl",
    "settlement_ms": 1787619600000,
    "as_of_ms": 1787619300000,
    "dataset": "data",
    "output": "json",
    "tracing": "normal"
  }
}
```

Exit codes are stable: 0 success, 1 internal, 2 command usage, 3 unimplemented,
10 configuration, 11 network, 12 schema, 13 storage, 14 partial data, 15 output
failure, and 130 cancellation. Error messages begin with the matching category
name.

## Future wallet and trading extensibility

The architecture should make future execution possible without coupling it to
the read-only research core. Do not add keys or order placement to
`hypercarry-core` merely in anticipation of trading.

A safe future shape is:

```text
market data -> normalized dataset -> metrics/prediction -> strategy intent
                                                        -> risk policy
                                                        -> execution adapter
                                                        -> signer/wallet
```

Recommended boundaries:

- keep market data, storage, and metrics usable without wallet dependencies;
- represent a proposed order as an inert typed intent before it reaches an
  execution adapter;
- define signer and execution traits in a separate crate or optional feature;
- use established wallet/signing providers rather than inventing key storage;
- default to no execution, then testnet or dry-run, with mainnet requiring an
  explicit configuration and confirmation boundary;
- require limits for market, notional, leverage, slippage, frequency, and daily
  loss before an order can be submitted;
- use idempotent client order IDs, reconciliation, an audit log, and a kill
  switch;
- never persist raw private keys in project configuration or the dataset;
- test execution adapters against simulations and testnet before mainnet.

This preserves an upgrade path without expanding the present threat model or
making a read-only analytics tool dangerous by default.

## Current verification

Normal verification at the current revision:

```sh
tools/check-rust-supply-chain.sh
cargo deny check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo build --workspace --all-features --release --locked
```

Current normal all-feature test result: **171 passed, 0 failed, 3 intentionally
ignored**.
The ignored tests are the opt-in mainnet/testnet REST checks and testnet live
WebSocket recorder smoke check. All normal recorder/replay tests are offline.

## Recommended next moves

1. Have an independent human inspect the clean candidate artifacts and use the
   offline `review` command to emit the first immutable reviewer attestation.
2. Record two more independently reviewed clean credentialed sessions and run
   the documented disconnect, restart, private-stream, and fault exercises.
3. Only after those sessions pass, implement and freeze the exact future
   mainnet transport, then review the complete bundle before human approval.

## Keeping this file current

For every material pull request:

1. Update the capability table and milestone state if behavior changed.
2. Move completed work out of “Recommended next moves.”
3. Update the verification result if tests were added, removed, ignored, or
   changed.
4. Record new architectural or safety decisions in the relevant section.
5. Keep planned behavior clearly separated from behavior that exists now.
6. Update the “Last reviewed” date.
