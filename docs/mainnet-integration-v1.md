# Mainnet integration v1 — implemented, release closed

The mainnet adapter and manual executor can be built and tested while the
independent testnet reviews continue. Building this code does not authorize
trading. No live mainnet orders were submitted during implementation.

## Crates and feature isolation

- `hypercarry-execution`: venue-neutral risk, identity, lifecycle, durable
  journal and the default-off `mainnet-execution` release gate.
- `hypercarry-hyperliquid`: the migrated testnet implementation, SDK signing
  boundary, wire parsing, private streams and capability-gated mainnet adapter.
  `testnet-execution` and `mainnet-execution` are default-off. Mainnet enables the
  pinned SDK for independent signature recovery. No dependency versions were
  upgraded as part of this split.
- `hypercarry-executor`: non-published Unix binary, built only with
  `mainnet-execution`. It offers `config-digest`, `preflight`, `run`, and
  `recover`; it has no key loader or arbitrary endpoint option,
  automatic strategy loop, leverage changes, vaults or transfers.

The public analytics CLI has no execution dependency. Existing testnet library
users must change venue-specific imports from `hypercarry_execution` to
`hypercarry_hyperliquid` and enable its `testnet-execution` / `hypersdk-signer`
features. The existing testnet operator and Foundry provider were migrated;
their network and signing behavior remain testnet-only.

```sh
cargo test -p hypercarry-hyperliquid --features testnet-execution --locked
cargo test -p hypercarry-hyperliquid --all-features --locked
cargo build -p hypercarry-executor --release --features mainnet-execution --locked
./target/release/hypercarry-executor --help
```

## Submission and recovery boundary

`HyperliquidMainnetExecutor::place` takes `MainnetAuthorization` by value and
immediately consumes it. It accepts no second order. The adapter checks the
same release digest, pinned signer alias/network, canary limits, fresh health,
and watchdog before signing and again after signing. It checks capability and
signed-action deadlines plus the kill switch at the final durable submission
boundary. The SDK independently recovers the signature against the mainnet L1
domain and pinned agent address. Signed payloads and signatures never enter the
journal. The HTTPS exchange transport is private, fixed to the official mainnet
endpoint, rejects redirects and bounds timeouts and response reads.

Only one exact GTC limit canary is supported. The optional MCP interface can
consume one-use operator consent via `run-approved`; all execution gates remain
in place. See [MCP consent](mcp-v1.md#one-use-mainnet-consent). Its size, price and identity are
part of the reviewed runtime digest. Live metadata must match the reviewed
asset index and size precision; price and size must satisfy venue precision.
The runner requires an independently attested flat account across all DEXs,
plus its own checks for a flat default-perp account and zero default-DEX/spot
open orders before placement. It rechecks agent authorization, standard/unified
account collateral and open orders before submission. Other perp DEXs,
subaccounts, vaults and portfolios are outside this canary scope.

The executor journals a network/account/config/evidence/signer context before
an action. `run` requires an unused journal and never automatically retries
placement. Ambiguous acknowledgements are durably marked uncertain. `recover`
requires the matching context and exact mainnet action, replays lifecycle
state, queries by client order ID, cancels remaining open quantity, and polls
within reviewed bounds. Unknown orders remain unresolved; the executor never
assumes a timeout means rejection or generates a replacement order. Retain all
failed journals. A failure can require the separately reviewed emergency venue
procedure if REST cannot establish a recoverable state.

SIGINT/SIGTERM block new submission and allow the bounded cancel/reconcile path
to finish. Cancellation remains available with stale placement health or an
engaged kill switch, but still requires the pinned signer and valid static
release/build identity. Keep the exact reviewed binary and config for recovery.
Abrupt process/host failure requires the independent watchdog. The final JSON
includes the client order ID, state, cumulative filled quantity and an
independent default-DEX/spot zero-open-order check. **Cancellation does not flatten fills:** a
filled position remains in the account and needs the reviewed operator response.

## Release preparation

1. Complete the three independently reviewed credentialed testnet sessions and
   fault exercises. Do not count public REST/WebSocket smoke checks as execution
   evidence. Obtain the security and rollback reviews required by the release
   gate.
2. Commit the intended source and build from a clean tree. The binary embeds
   the source commit, lockfile digest and clean-build flag. Uncommitted builds
   can run help/digest/tests but cannot pass mainnet preflight. Preserve the
   binary; put `sha256/<binary SHA-256>` in the reviewed bundle's
   `mainnet_transport_artifact` field.
3. Copy [the runtime example](mainnet-runtime-config.example.json) outside the
   repository and replace every illustrative identity, path, order and limit.
   These are schema examples, not approved prices, accounts or production risk
   settings. Runtime paths must be distinct absolute paths under private 0700
   directories, without symlinks or parent traversal.
4. Obtain the canonical digest without signing or network I/O:

   ```sh
   ./target/release/hypercarry-executor config-digest --runtime /private/canary/runtime.json
   ```

5. Assemble `config.json` as `{"schema_version":1,"gate":{...},"runtime":{...}}`.
   The `gate` object uses `MainnetReleaseConfig` from
   [the release gate](mainnet-release-gate-v1.md). Set its
   `integration_config_digest` to the runtime digest. Bind `gate.review_digest()`
   into the reviewed bundle's `config_digest`. The optional field is omitted
   for historical gate fixtures; the executor requires it. Any account, signer,
   path, risk, order, timeout or asset change changes the runtime digest and
   requires a newly reviewed release bundle.
6. Assemble the real schema-v2 evidence and obtain an independent human release
   decision binding its digest. This binary deliberately does not generate
   approving evidence or fabricate operational readiness.
7. Validate the frozen build and static evidence offline:

   ```sh
   ./target/release/hypercarry-executor preflight --config /private/canary/config.json --evidence /private/canary/release.json
   ```

   Preflight does not contact the signer/venue or declare runtime health ready.
   When the reviewed external services below are actually running, `run` also
   requires `--enable-mainnet` and an interactive terminal containing the exact
   phrase `ENABLE HYPERCARRY MAINNET CANARY`. Recovery uses the `recover`
   subcommand with the same config/evidence and the separate interactive phrase
   `RECOVER HYPERCARRY MAINNET ORDERS`.

## Required external operational services

The integration now ships a mainnet signer service and watchdog daemon in
`hypercarry-mainnet-services`; see [operational services](mainnet-services-v1.md).
Their external custody backend, independent hosts, protected transports and
alert delivery must still be provisioned, exercised and reviewed. The
supplied Foundry signer is still testnet-only; do not point it at mainnet.

The health supervisor is **partly** shipped, as `hypercarry-supervisor`. It
establishes account flatness across every perp DEX, equity, open orders,
reference price, and signed realized rolling P&L, and writes the health file
atomically. It holds no credentials and cannot sign: its dependency tree
contains no SDK, venue adapter or signing stack. Everything it has not yet
measured — startup and continuous reconciliation, the private stream and its
latency, alerts, the audit journal, rollback — it writes as `not_ready`, so the
executor declines. Running it today therefore produces a valid health file that
authorizes nothing, which is the intended behaviour for an incomplete
supervisor. See the section below for the full requirement it is working
towards.

- A protected external mainnet L1 agent signer implements the owner-only Unix
  socket protocol from [the testnet operator](testnet-operator-v1.md), with
  explicit `network: "mainnet"` in both request and response and the separately
  pinned mainnet alias/address. The provider must sign the exact ordered L1
  action through the reviewed SDK, including expiry and mainnet domain, then
  close its response stream. Never put a key/password in config or environment.
- An independent supervisor maintains the private `orderUpdates`/`userFills`
  stream, reconciles journal/account state on startup and reconnect, checks
  alerts/audit/rollback, and atomically replaces the health file. The adapter
  exports `connect_mainnet_private_stream` and normalized private-event handling
  for this integration. The one-shot binary uses REST for its own lifecycle;
  it does not invent private-stream readiness from that REST result.
- A separate watchdog must perform the reviewed emergency response if the
  executor/host fails. Its fresh heartbeat is observed by `FileDeadManSwitch`.
  Merely touching a file does not implement emergency cancellation.

  The watchdog must also be *independently effective*, which a host-local
  process cannot be on its own: if the host is terminated, partitioned, or
  wedged, the cancellation it was going to perform never happens. The adapter
  therefore exposes `schedule_cancel` — Hyperliquid's venue-side scheduled
  cancel — which leaves the instruction with the venue, where it executes
  regardless of this host's fate. Arm it on a deadline and re-arm well inside
  that horizon; ceasing to run is then what triggers it. Venue rules: the
  deadline must be at least five seconds ahead (checked locally before
  submitting), omitting the time disarms, and the venue permits at most ten
  *triggers* per day, reset at 00:00 UTC — re-arming does not consume that
  budget, firing does.

  **Scheduled cancel cancels open orders. It does not close positions.** An
  account holding a filled carry leg remains fully exposed after it fires, so
  the emergency response is not complete without a separate, reviewed procedure
  for flattening. "No open orders" and "flat" are different claims and the
  recovery path must establish them separately — `account_flat` in the health
  file is the latter, and it must reflect positions across all DEXs.

  Arming and disarming are journaled as `emergency_cancel_scheduled`, recorded
  only after the venue confirms, so the journal never claims protection that is
  not in place. Absence of the event is not evidence that nothing is armed: a
  prior process may have armed it, so recovery re-establishes the state rather
  than inferring it.
- Persistent journal storage, clock synchronization, restricted OS identities,
  working alerts and a retained rollback/recovery binary complete the host
  setup. A database service and a remote order API are unnecessary.

The strict health file is a schema-v1 object with `network: "mainnet"`,
`account_address`, `integration_config_digest`, `account_flat: true`,
`health: OperationalHealth` and
`risk: RiskSnapshot`. Both nested objects use the exact Rust field names;
readiness is `"ready"` or `"not_ready"`, and risk decimals are strings. Health
and risk observation timestamps must match. Include every health field
(startup/continuous reconciliation, unmanaged/unresolved counts, REST/private
latency, private-stream/alerts/audit/rollback readiness). The risk snapshot must
include actual aggregate exposure, equity, recent order times and rolling PnL.
`account_flat` must reflect positions across all DEXs; the local REST query
alone does not establish that for unified accounts. Missing, malformed, stale,
future-dated or wrongly bound snapshots fail closed.
The executor additionally measures the complete live account-check REST batch
and checks freshness again after that I/O. Configure latency/TTL thresholds
based on reviewed testnet measurements, including external signer time.

The supervisor and its filesystem authority are trusted operational inputs,
not cryptographically attested observations. Their configuration and behavior
belong in the independent execution security review. Do not hand-author a
`"ready"` file to bypass missing services.

## Venue references

Wire endpoints and precision checks follow the official
[API](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api),
[tick/lot rules](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/tick-and-lot-size)
[account-query scope](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint)
and [signing guidance](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/signing).
The integration retains pinned `hypersdk = 0.2.15` and tests recovery of real
SDK signatures using a public deterministic test key only in offline fixtures.
