# Hypercarry development status

Last reviewed: **2026-10-01**

This is the living engineering-status document for hypercarry. Update it when a
capability lands, a milestone changes state, or a verification command changes.
`README.md` describes the product. The public operational requirements are in
[the release gate](docs/mainnet-release-gate-v1.md) and
[the integration runbook](docs/mainnet-integration-v1.md).

## Upstream drift: stable Clippy and Arrow/Parquet 60 — 2026-10-01

Two maintenance items, both caused by upstream moving rather than by this code.

CI pins no toolchain for its Clippy step — it tracks floating stable with
`-D warnings` — so a new lint becomes a build failure the day it ships. Rust
1.99 added `clippy::assert_is_empty`, which turned seventeen
`assert!(x.is_empty())` sites across seven crates into hard errors. Rather than
take Clippy's `assert_eq!(x, [] as [T; 0])` rewrite, each assertion gained a
message that prints what was actually present — the rows, requests, journal
events or captured stdout that were supposed to be absent. That satisfies the
lint by answering it, and the failures are more useful than before. Reproducing
this locally requires the same floating stable Clippy that CI resolves, not
whatever toolchain happens to be installed.

`parquet`, `arrow-array` and `arrow-schema` move to 60.0 together. They have to
move together: `parquet` 60 requires Arrow 60, so bumping it alone would have
resolved two Arrow major versions at once and the column types would no longer
unify. This is why the dependency bump had to land here rather than as a bump
against the public mirror. No source change was required. Backward compatibility
was checked against real on-disk data rather than assumed: the committed mainnet
dataset, written by 59, still reads as 8,760 observations across 8,760
contiguous hours. MSRV 1.93, `cargo deny` and the supply-chain deny-list all
still pass.

## Post-implementation audit of the operational services — 2026-10-01

A skeptical review of the day's commits found four defects, all fixed here.

The watchdog re-armed the venue-side scheduled cancel on **every** lease poll,
so a reviewed cadence of 100ms–1s produced one exchange action and two agent
authority checks per poll indefinitely. Venue actions are rate limited per
address, so a long session would have spent the budget the emergency cancel
itself depends on. Re-arming is now paced by a separate reviewed
`rearm_interval_ms`, capped at a third of the cancel horizon, while the lease is
still polled every interval. A heartbeat is written only while the armed
deadline outlasts the executor's heartbeat timeout, so the file can never
promise cover the venue has stopped holding. Both decisions are pure, unit
tested predicates (`rearm_due`, `heartbeat_permitted`) rather than inline
conditions. Confirming the venue's address action budget against the chosen
cadence is now an explicit operational-review item.

The emergency price collar was not checked against the reviewed price tick.
`flatten_action` rejects an unquantized price, so an unaligned bound would have
failed only while flattening a live position. The policy validator now refuses
it at load.

The MCP server annotated a pinned command's result with provenance keys without
checking the result was a JSON object. An array or scalar would have panicked
the server *after* the command ran — for `submit_reviewed_canary`, losing the
report of a completed mainnet order. `process::run` now requires an object.

The signer's socket cleanup used `?`, which could replace the reason the accept
loop stopped — the reason an operator reconciles retained nonce state against.
The loop's error now wins. The agent authority check also took a freshly built
blocking HTTP client per call, starting a runtime thread and loading a root
store twice per action on the emergency path; it now takes a shared client.

Validation after the audit: **281 passed, zero failed, four opt-in live tests
ignored**. Strict all-feature Clippy, formatting, release build and default-off
build all pass.

What this audit found but deliberately did not change: the venue's actual
per-address action budget is still unverified against the chosen cadence; the
ordering between the executor's own `recover` and a running watchdog is a
runbook decision, since recovery starts no liveness lease and an armed watchdog
reads that as executor loss; the lease and heartbeat files are fsynced on every
renewal, which is write amplification the replication transport has to absorb
rather than a code defect; and supervisor readiness refuses any venue history at
the 2,000-record cap, which is correct but eventually unusable for a long-lived
account. Three smaller MCP items remain open: protocol-version negotiation,
canonicalizing storage paths that do not yet exist, and whether
`get_dataset_health` should stay an alias of `get_funding_apr`.

## Documentation and production handoff — 2026-10-01

Refreshed the README and maintainer beginner/technical/Q&A guides for scoped MCP,
external custody and independent recovery. `TODO.md` now separates provider
implementation, service transport, unfinished supervisor observations, independent
session evidence and release freeze/approval, with explicit acceptance criteria.
The public description is “Hyperliquid funding recorder, predictor, and execution
research tool”; changing the later public mirror's metadata remains deferred.
Mainnet readiness stays closed. The validation figures below belong to the last
implementation (`ff8404c`); documentation work does not create live evidence.

## Operational failure-path verification — 2026-10-01

Final review bounded backend stdout closure after process exit (including a
provider leaving a pipe open), added real mainnet-domain signature recovery and
wrong-identity/timeout fixtures using only a public deterministic test key, and
rechecks lease/STOP after watchdog signing/venue I/O before renewing heartbeat.
Emergency signing/submission also rechecks the agent-to-account binding without
requiring the filled account to be flat. This prevents a reassigned agent from
silently targeting a different account during recovery.

Final workspace validation: **275 passed, zero failed, four opt-in live tests
ignored**. All-feature strict Clippy and Rust 1.93 all-target/all-feature checks
passed. Default-off check, release build, formatting, cached cargo-deny and
supply-chain checks also passed; the final release build is repeated from the
clean committed tree so its embedded source identity is usable for review.

## Gated MCP execution — 2026-10-01

The owner authorized read/write MCP with gated mainnet orders. `trade` is a
separate explicit launch scope and requires `--enable-mainnet`. The operator's
`approve-mcp` command displays the exact configured action, verifies static
release/build evidence, and issues a 1..300-second private approval only after
interactive confirmation. `run-approved` atomically consumes it, retains a
non-overwritable claim and follows the existing live execution/recovery gates.
MCP accepts only an approval ID, never another order, account, path or endpoint.
The default analytics dependency graph remains free of signing code.

Validation: targeted approval tests cover expiry, operation/evidence binding,
replay and concurrent consumption. MCP subprocess tests verify real stdio
handshake, local history, denied writes and explicit mainnet launch requirements.
Mainnet readiness is still closed pending real provisioning, supervisor readiness
and independent release evidence; permission to build does not establish those.

## Scoped MCP analytics — 2026-10-01

`hypercarry-mcp` provides an optional stdio MCP process, read-only by default.
Read tools expose snapshots, history, APR/data health, carry evaluation and
causal predictions. Explicit write scope enables bounded backfill and validated,
non-overwriting trade records. Network/coin/path/executable restrictions are
launch-time settings, enforced at tool discovery and dispatch. JSON envelopes
preserve decimal strings, observation time, units, coverage, assumptions and
provenance; prediction confidence explicitly means data completeness.

Validation: 7 MCP tests pass, including real Parquet history/pagination gaps,
read/write denial, path/network overrides, notification mutation refusal and
child timeout/hash enforcement. Strict all-target MCP Clippy passes.
See `docs/mcp-v1.md` for integration and scope contracts.

## External operational services — 2026-10-01

Implemented default-off `hypercarry-mainnet-services`: owner-only signer with
mainnet-domain signature recovery, exact-order/emergency policy, hash-pinned
external custody backend and durable nonce/one-canary reservation; independent
watchdog with venue-acknowledged re-arming, executor-owned lease and bounded
reduce-only IOC recovery followed by position verification. Policy/artifact
identities bind through optional runtime fields; historical digests remain
unchanged. `hypercarry-executor` now exposes its config/safety/signer library.
See `docs/mainnet-services-v1.md` for provisioning and failure-domain limits.

The service code and deployment templates exist; no production key backend,
hosts, forwarding/replication or alert integration have been provisioned. The
supervisor and independent evidence remain outstanding. User authorization to
implement and allow mainnet orders is recorded in the conversation, not treated
as fabricated testnet/release evidence. Release readiness remains closed.

Validation: targeted executor/config/services tests and strict Clippy. No
credentials loaded and no live orders or emergency actions submitted.

## Order-frequency correction — 2026-10-01

The supervisor now reads `historicalOrders` and counts each venue order ID once
using its original submission timestamp and the risk policy's frequency window.
Canceled/rejected/unfilled orders count; partial fills and status updates do not
create submissions. Conflicting/future timestamps and saturated 2,000-record
histories fail closed. Fill history is used only for realized P&L and also
refuses a saturated response. Uncertain local attempts still require journal
reconciliation; no readiness field was enabled by this correction.

Validation: 12 supervisor tests passed, including duplicates, canceled/unfilled
orders, inclusive boundaries, malformed timestamps, and truncated histories.

## Mainnet integration implementation — 2026-09-06

Implemented on the existing local review branch at the operator's request,
while credentialed testnet evidence remains outstanding. This changes the
previous build ordering, not the live release conditions.

- Moved Hyperliquid-specific testnet code and SDK dependencies out of
  `hypercarry-execution` into new `hypercarry-hyperliquid`, preserving the
  testnet operator and its offline coverage. The analytics CLI still has no
  execution or signing dependency.
- Added the default-off mainnet facade with a private fixed-endpoint HTTPS
  transport, exact consumed order authorization, live gate/risk/deadline checks,
  SDK mainnet-domain signature recovery, durable uncertain outcomes and strict
  reconciliation identity checks. Clock sampling happens after blocking health
  I/O; expiry is checked again after the durable pre-submission journal write.
- Added the separate non-published `hypercarry-executor`: canonical config digest,
  offline static preflight, interactive one-shot canary and journal recovery.
  It binds complete runtime config, compiled source/lock/binary identity and
  journal account/release context, uses a protected external signer, checks live
  account/asset data, handles shutdown and reports cumulative fills explicitly.
- Documented the required external mainnet signer, independent private-stream
  health supervisor, alert delivery and watchdog response. These services are
  not supplied by the new binary and remain part of the release-readiness work.

Verification: all-feature workspace tests (224 passed, four ignored), strict
Clippy, formatting, all-feature release build, Rust 1.93 all-target/all-feature
check, supply-chain policy and dependency audit passed. Default-off and
separate testnet feature builds passed. The read-only CLI dependency tree
contains no execution/venue adapter/SDK signing dependency. The two offline
executor subprocess checks reject missing explicit enablement and incomplete
review evidence before journal creation or any signing.

Public testnet REST (all three info requests) and ten-second WebSocket smoke
checks passed during this work. They do not count as credentialed testnet
sessions. Mainnet placement was tested only with offline fixture transports
and a public deterministic SDK test key. No real mainnet or testnet orders
were submitted, no production approval was emitted, and no credentials were
loaded. See [mainnet integration v1](docs/mainnet-integration-v1.md) for build,
configuration, API migration, recovery behavior and deployment requirements.

## Venue-side scheduled cancel — 2026-09-30

`schedule_cancel` arms Hyperliquid's scheduled cancel through the adapter, on
both the testnet executor and the mainnet facade. It exists because a host-local
watchdog cannot be independently effective on its own: if the host is
terminated, partitioned or wedged, the cancellation it was going to perform
never happens. Arming leaves the instruction with the venue, which executes it
regardless of this host's fate.

It is deliberately not gated behind a `MainnetAuthorization`. That capability
makes *adding* risk a single reviewed non-repeatable act; this only removes
risk, and requiring permission to protect the account would withhold protection
in exactly the conditions — unhealthy runtime, engaged kill switch, expired
evidence — that call for it.

**It cancels open orders and does not close positions.** An account holding a
filled leg is still exposed after it fires, so the emergency response is
incomplete without a separate reviewed flattening procedure.

Venue rules are enforced or recorded, not assumed: a deadline under five seconds
ahead is refused locally rather than spent as a rejected request, omitting the
time disarms, and the ten-triggers-per-day budget is documented on the exported
`SCHEDULE_CANCEL_MAX_TRIGGERS_PER_DAY` constant. Arming and disarming journal
`emergency_cancel_scheduled` only after the venue confirms, so the journal never
claims protection that is not in place; an uncertain write returns an error
rather than success.

## Account-health supervisor — 2026-09-30 (in progress)

`hypercarry-supervisor` is the independent supervisor the integration runbook
requires. It is the process that writes the health file the executor refuses to
act without, and nothing else in this workspace produces that file.

Two properties are structural rather than documented. It **holds no
credentials**: the private `orderUpdates`/`userFills` subscriptions are unsigned
and take an account address, and the REST info queries are address-scoped, so
there is no key and no signing path. Its dependency tree contains zero
occurrences of the SDK, the venue adapter or the signing stack — the executor's
contains 141 — and it reads the clock from `std` rather than borrowing the venue
adapter's, so the graph itself shows it cannot trade.

It **never claims what it has not measured**. Every readiness field must be
`ready` for the gate to authorize, so a field with no source behind it is
written `not_ready` and the executor declines. That is what makes the partially
built state safe: it withholds authorization rather than granting it on
incomplete evidence. A test asserts that what this binary writes today
authorizes nothing, so a field cannot start reporting ready without a source.

Established now: flatness across **every** perp DEX — enumerated from
`perpDexs`, where the default DEX is a literal `null` entry — which is the claim
the executor cannot make for itself, since its own check covers the default DEX
only and that does not establish flatness for a unified account. Also account
equity, open order count and aggregate notional, reference mid price, and signed
realized rolling `PnL` with its fill times, computed from `userFills` so it
survives this process restarting.

Still `not_ready`, each needing its own observed source: startup and continuous
reconciliation against the durable journal, the private stream, alert delivery,
the audit journal, and rollback. Supervisor settings live in their own
configuration file and must never move into `RuntimeConfig`, whose serialization
is the reviewed digest.

## Shared mainnet runtime configuration — 2026-09-30

`RuntimeConfig` and its reviewed digest moved out of the bin-only
`hypercarry-executor` into a new `hypercarry-mainnet-config` library. The
executor rejects a health file whose `integration_config_digest` differs from
its own, so the independent supervisor that writes that file has to compute the
identical value. The digest hashes the type's JSON serialization, which makes
it sensitive to field order and serde attributes — two independent declarations
of the same shape would agree until the day one of them gained a field. Holding
the definition once removes that failure mode by construction.

The new crate has no venue, signer or transport dependency and cannot trade. It
deliberately does not carry the configuration envelope: `MainnetReleaseConfig`
is gated behind `mainnet-execution`, so taking only the runtime section keeps
the crate free of that gate and leaves the workspace's default-off build
unchanged. The envelope and all execution policy — `validate`, `verify_build` —
stay in the executor.

The health file moved for the same reason, into a `health` module gated behind
`mainnet-execution`. It is a wire contract between two processes and only the
reader declared it: the supervisor that writes it would have matched the shape
by hand, and a field added on one side would have compiled cleanly and then
rejected every health file at runtime. `HealthEnvelope` now carries its own
binding check — schema, network, account, digest, and the requirement that
health and risk describe a single instant, so a stale half cannot hide behind a
fresh one — plus an atomic owner-only write. The executor keeps what only it can
do: re-reading at the authorization boundary, measuring its own live
account-check latency, re-testing freshness after that I/O, and requiring the
supervisor's `account_flat` claim.

A pinned-digest test asserts that a fixed configuration hashes to the value
captured before the move. Every reviewed release bundle records the digest it
approved, so a field reorder would silently invalidate all of them at the moment
an operator most needs the binding to hold; the test turns that into a failure.

## Repository review and public mainnet verification — 2026-09-06

Focused review of the read-only CLI, REST history validation, live recorder
coverage, CI requirements, and execution-readiness documentation produced three
improvements:

1. Snapshot history now uses the existing validated, retry-aware range reader.
   Wrong-coin, out-of-window, and conflicting settlement rows fail with schema
   exit code 12; rows are sorted and identical overlaps collapse. Regression
   coverage exercises each invalid case. The previous snapshot fixture itself
   contained settlements outside its requested window; it now supplies bounded
   synthetic history while retaining recorded context and prediction fixtures.
2. CLI tracing explicitly writes to stderr, preserving the documented single
   JSON document on stdout even with diagnostic tracing enabled.
3. Public recorder smoke coverage now includes mainnet alongside testnet. Both
   tests cancel after ten seconds and enforce a thirty-second outer deadline
   covering recording and shutdown. They remain ignored in normal offline runs.

Verification on this checkout:

- `cargo test --workspace --all-features --locked`: 172 passed, zero failed,
  four opt-in live tests ignored. Local Unix socket tests required execution
  outside the filesystem sandbox.
- `cargo fmt --all -- --check`, strict all-target/all-feature Clippy, the locked
  all-feature release build, supply-chain deny-list check, and `cargo deny check`
  passed. The dependency audit required access to its advisory database.
- `cargo test -p hypercarry-core --test live_info mainnet_info_smoke --locked -- --ignored --nocapture`:
  passed against public mainnet REST (context, predictions, BTC history).
- `cargo test -p hypercarry-recorder --test live_recorder mainnet_live_recorder_smoke --locked -- --ignored --nocapture`:
  passed; connected and received raw and normalized data during ten seconds.
- Release CLI `snapshot --network mainnet --coin BTC --output json --tracing diagnostic`:
  passed; stdout parsed as one JSON document with four BTC settlements inside
  the reported window, while diagnostic logs appeared on stderr.

Live checks required network access outside the sandbox. These results establish
public market-data connectivity only, not trading readiness or sustained service
reliability. No orders were submitted. At that review, mainnet execution remained unavailable. The subsequent
integration work below supplies the adapter and binary; independent testnet
reviews, operational services, security review and final release approval are
still outstanding. This was a focused
repository review, not a comprehensive security audit. Live output and datasets
were kept outside version control.

## Product goal

Hypercarry is a local-first Hyperliquid funding research application. It is
intended to collect public perpetual-market data, retain a reproducible history,
calculate carry metrics, evaluate a next-hour funding estimate, and eventually
present the results through a polished command-line and terminal interface.

The default CLI is deliberately read-only. It does not accept private keys,
sign requests, access a wallet, or place orders. The optional execution library
can place testnet orders only through a caller-supplied external signer; it has
no built-in key loader. The separate default-off mainnet integration is now
implemented, but live release readiness remains closed.

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
| Mainnet integration | Implemented, default-off and unreleased | Exact-order manual/MCP-consent executor; isolated signer and emergency watchdog implemented; custody backend, independent hosts, supervisor completion and release evidence remain outstanding |
| MCP analytics/local writes | Implemented, opt-in process | Read-only default; separate write scope; strict structured metadata, pinned executables and storage boundaries |
| MCP trading | Implemented, gated | Explicit trade scope and mainnet flag; expiring one-use operator consent; same release/build/live gates |

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
  and states how much of the history is unbroken, not just how much exists.
  `contiguous_history_hours` measures the run ending at the newest observation
  and is carried in JSON so a monitor can assert recent history is complete; an
  old gap no longer erases the recent run. Empty streams produce an actionable
  partial-data error.
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
  connection recycling, and a clean close boundary on both `SIGINT` and
  `SIGTERM`. A service manager stops a unit with `SIGTERM`, so listening for
  Ctrl-C alone meant every supervised restart killed the recorder before the
  session's normalized Parquet was finalized, discarding the analytical
  projection and leaving only raw JSONL to replay.
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

M9 originally added the separate default-off `mainnet-execution` release-gate
module. The subsequent mainnet integration now supplies the adapter and binary;
its operational release remains closed. Schema-v2 evidence cryptographically binds repeated clean
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

Current normal all-feature test result: **281 passed, 0 failed, 4 intentionally
ignored** (includes executor and MCP subprocess integration checks).
The ignored tests are the opt-in mainnet/testnet REST checks and mainnet/testnet
live WebSocket recorder smoke checks. All normal recorder/replay tests are offline.

## Carry P&L ledger

`hypercarry-core`'s `carry` module values one carry trade — a perpetual leg plus
an optional opposing spot hedge — over a sequence of settled funding
observations, and reports funding accrual, per-leg price result, fees, and net.
It closes a real gap: `predict` scored prediction error and `rolling_pnl` in
`hypercarry-execution` is an input read from an external health file, so until
now nothing in the workspace computed strategy return.

The module is pure, offline, and exact. Every fund-affecting operation is
checked, and the ledger fails closed on repeated or out-of-order settlements
rather than double-counting funding. It deliberately does not invent a
per-settlement valuation price: the caller supplies one, so a constant-notional
approximation is visible at the call site instead of hidden in the arithmetic.

The read-only `pnl` command replays the settled-funding Parquet dataset through
it. The position is a schema-v1 trade document the operator records, not
pipeline output, so the CLI gains no execution capability. Funding applies to
settlements strictly after entry and at or before exit; `window_fully_covered`
requires observations bracketing the window *and* an unbroken hourly sequence
between them, because a settlement missing inside the window understates funding
just as a missing endpoint does, and a span check cannot see it. It is reported
for closed trades: an open position is never fully covered. The contract is
documented in `docs/cli-contracts-v1.md`.

## Recommended next moves

Track 1 (live purpose without execution) and Track 2 (release readiness) proceed
in parallel; see `TODO.md`, whose "Start here" section carries the concrete next
action. Funding settles hourly, so capturing carry does not require the
executor, and automating execution is an efficiency project rather than the
thing that makes the repository operationally live.

Most remaining work is operator work: a host to run capture on, a human
reviewer, and external services to provision. This section previously claimed
that *no* code blocked any of it, which was too strong — preparing the
deployment surfaced three code defects that did: the recorder was killed by a
service manager's `SIGTERM` before finalizing its session, `apr` reported a
contiguous-history duration only when the dataset was an exact multiple of 24
records, and `pnl` called a window fully covered when a settlement inside it was
missing. All three are fixed. The supervisor is the one remaining item that is
still partly implementation; everything else is provisioning, evidence and
approval.

1. Install the recording host from `deploy/`, which packages the hourly
   per-coin `backfill` timer, the freshness/contiguity/disk health check, the
   on-failure alert hook and the optional recorder unit. Then record the first
   manually executed carry position through the `pnl` ledger. This is the next
   concrete action. Note that `backfill` is the only writer of settled funding;
   `record` captures a separate, much larger layer and is not required for
   carry P&L.
2. Have an independent human inspect the clean candidate artifacts and use the
   offline `review` command to emit the first immutable reviewer attestation.
   Track 2 is blocked on this, not on implementation.
3. Record two more independently reviewed clean credentialed sessions and run
   the documented disconnect, restart, private-stream, and fault exercises.
4. Finish `hypercarry-supervisor`: reconciliation against the durable journal
   first, then the private stream, then whatever will attest alerts, audit and
   rollback. Until each readiness field has an observed source it stays
   `not_ready` and the executor declines, which is the intended behaviour.
5. Provision and review the external mainnet signer and the watchdog driving the
   emergency response; freeze the implemented adapter/executor and complete
   release bundle after the testnet evidence passes, before human approval.
6. Sync the public mirror with `tools/export-public.sh --push`. It has never
   been synced; the dry run is clean and the two blockers identified before the
   first sync (the `cargo deny` advisory and the README structure) are cleared.

## Keeping this file current

For every material pull request:

1. Update the capability table and milestone state if behavior changed.
2. Move completed work out of “Recommended next moves.”
3. Update the verification result if tests were added, removed, ignored, or
   changed.
4. Record new architectural or safety decisions in the relevant section.
5. Keep planned behavior clearly separated from behavior that exists now.
6. Update the “Last reviewed” date.

`Z-Explainer.md` carries its own checklist for the structural claims it owns.
Two mechanisms support both files rather than relying on memory:

- `tools/hooks/pre-commit` blocks a commit that changes code without staging
  this file, and one that changes structure without staging the explainer. It
  prints the relevant checklist. Install with
  `git config core.hooksPath tools/hooks`; bypass deliberately with
  `SKIP_DOC_CHECK=1` for a work-in-progress commit you intend to amend.
- `tools/check-docs-current.sh` reports how many commits have touched `crates/`
  since each document was last updated, so a bypassed hook still leaves a
  visible signal in CI and in a reviewer's terminal.
