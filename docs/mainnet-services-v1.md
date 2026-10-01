# Mainnet operational services v1

Implemented services are not a provisioned deployment or release approval.
The user authorized mainnet implementation and execution on 2026-10-01. Actual
execution still requires an exact runtime, independent evidence, healthy
supervision and the reviewed release digest. None has been invented here.

## Dependency order and scope

1. Correct submission-frequency accounting (implemented).
2. Build the isolated signer and independent watchdog (implemented here).
3. Add MCP reads/local writes, then single-use approved trading.
4. Provision the chosen key backend and separate hosts; finish observed
   supervisor readiness; complete independent testnet/security/rollback review.
5. Build from the final clean commit, freeze artifacts/configuration and approve
   that digest. Run the bounded canary only after preflight and live gates pass.

`hypercarry-mainnet-services` supplies `hypercarry-mainnet-signer` and
`hypercarry-watchdog`, both behind default-off `mainnet-execution`. The executor
now exposes shared configuration, signer client and safety checks as a library.
The analytics CLI gains no trading dependency.

## Signer

The service implements the existing owner-only Unix socket protocol. It checks
mainnet identity, expiry, one exact reviewed GTC order, its exact cancellation,
bounded scheduled cancellation, or bounded opposite-side reduce-only IOC
recovery. No transfers, leverage changes, disarming or arbitrary orders exist.
Placement rechecks release/health/risk before and after provider signing.
Emergency actions also recheck current agent-to-account authorization without
requiring flatness; a reassigned agent must not target a different account.

A durable, exclusively locked state records the highest nonce and whether the
single placement was reserved. Reservation happens before calling the backend.
A timeout or lost response consumes the reservation: preserve state and
reconcile; never delete state to retry. Different reviewed releases need new
state paths. One service must own the agent nonce domain; do not share its key
with another frontend or trading process.

Key custody remains in a hash-pinned external executable. It receives one JSON
line on stdin:

```json
{"schema_version":1,"network":"mainnet","key_id":"provider/mainnet-canary","signer_address":"0x...","prehash":"0x..."}
```

It returns exactly one 65-byte recoverable Ethereum signature as a hexadecimal
string on stdout, then exits. The service computes the mainnet-domain prehash
with the pinned SDK and independently recovers the configured address. It never
logs provider output or signatures. The process has a cleared environment,
bounded output and timeout; the backend must not leave child processes running.
Use a hardware/KMS adapter appropriate to the operator's chosen custody system.
No backend credential, private key, password or fake production provider is
included. Choosing and configuring that backend requires the deployment owner.

## Watchdog and filled positions

Run the watchdog and signer in a failure domain independent of the executor
host. Protected Unix-socket forwarding and protected file replication must be
provisioned for the topology: expose only the pinned signer socket; replicate
the executor lease towards the watchdog and its heartbeat/kill switch towards
the executor. Preserve heartbeat modification timestamps. Do not run a timer
that touches copied files: stale copied state must remain stale. Host transport,
credentials and failover must be exercised during the operational review.

The executor itself renews its config-bound lease every 250ms, only while its
session exists. It waits up to 15 seconds for watchdog protection before placing
an order. Lease expiry, an engaged kill switch, failed re-arming or graceful
watchdog termination initiates recovery.

The watchdog polls that lease every `watchdog_interval_ms` but writes to the
venue only every `rearm_interval_ms`, which is capped at a third of the cancel
horizon. These are separate settings because a venue action is rate limited per
address while reading a local file is not: re-arming on every poll would spend,
over a long session, the same action budget the emergency cancel itself needs.
The reviewed bounds keep protection continuous — the horizon still standing when
a re-arm becomes due is at least two thirds of it, against a heartbeat timeout
below one half — and a heartbeat is written only while the armed deadline
outlasts that timeout. Confirm the venue's address action budget against the
chosen cadence during the operational review. The watchdog:

1. Persistently enters emergency state, sets STOP, withdraws its heartbeat.
2. Cancels the reviewed entry order; verifies open orders are gone.
3. Reads positions across all perp DEXs. Unexpected exposure is an incident.
4. Attempts one opposite-side reduce-only IOC, bounded by the original canary
   quantity and reviewed price collar. Venue reduce-only prevents reversal.
5. Re-queries positions. Remaining exposure is a failure needing operator action.

It does not claim a successful cancel means flat. Partial fills, unavailable
REST, or an exhausted price collar require recovery. `--recover` is an explicit
operator retry; it re-queries positions and never expands scope. Normal executor
completion withdraws its lease, so an outstanding canary fill also triggers this
recovery. Retain STOP, journals and state after every emergency.

Successful re-arm acknowledgement is required before writing the heartbeat.
Abrupt loss of the watchdog leaves the scheduled cancel at the venue. That
survives the host, but flattening requires a functioning recovery host, signer,
venue API and liquidity. Simultaneous loss of those systems cannot guarantee a
flat account. Provision an independent alert/on-call response for this case.

## Build, bind and install

```sh
cargo build --release -p hypercarry-mainnet-services -p hypercarry-executor --features mainnet-execution --locked
```

Copy the policy example and replace all paths, hashes and price bounds. Hash the
actual backend, signer and watchdog executables with SHA-256. Obtain the policy
digest without network access:

```sh
hypercarry-mainnet-signer --policy /etc/hypercarry-canary/services.json --policy-digest
```

Set `runtime.services_policy_digest` to that digest and `runtime.executor_lease`
to the policy's lease path. Recompute the runtime/gate digests and obtain release
review. These optional fields are absent from historical configurations, whose
existing digest remains unchanged. Changing service policy requires new approval.

The service unit templates in `deploy/mainnet/` are deliberately not installed
or enabled automatically. Create a private `hypercarry-canary` OS identity and
0700 state/configuration directories, install reviewed binaries and configuration,
and adapt provider device/network permissions. Install the templates on the
chosen hosts only after transports, alerts and the custody backend are tested.
The service does not automatically remove stale sockets or restart after an
emergency. Inspect and reconcile before restarting.

References: [exchange actions](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint),
[signer nonces](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/nonces-and-api-wallets).
