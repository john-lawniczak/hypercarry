# Single-canary supervisor

`hypercarry-supervisor run --config /etc/hypercarry-canary/supervisor.json`
maintains the executor health file. `sample` remains a diagnostic observation;
it cannot establish private-stream or operational-probe readiness.

The daemon holds no credentials and links no venue/signing SDK. It observes all
reported perp DEX positions **and open orders**, default account equity, mids,
realized closed P&L, venue order history, and the exact canary client order ID.
It checks the journal binding and replays lifecycle transitions. Only a pristine
journal or the exact not-yet-submitted action can become placement-ready. Any
submitted/uncertain/terminal action requires recovery and a new reviewed release
before another placement. This is deliberately not a general trading supervisor.
An order present at the venue cannot be hidden by deleting the local journal.
All outstanding orders are blockers for this flat-account canary; the reported
unmanaged count conservatively includes all of them.

The private connection requires both subscription acknowledgements, the initial
account fills snapshot, and fresh application pongs. Pong round-trip time is the
reported private transport latency. Private events (including duplicates),
disconnects, and reconnects invalidate the previous reconciliation. A new REST
batch must span an unchanged private-event epoch. Disconnects/events withdraw
the health file under the same lock used for publication. Failed observations
and graceful shutdown also withdraw it; abrupt process loss expires it by the
executor's reviewed age limit. Connect/read waits and every WebSocket write have deadlines, and messages
are limited to 1 MiB before allocation by the WebSocket transport.

Observation time is the **start** of the REST batch, not its completion. This
prevents slow batches or probes from making old prices appear newly observed.
DEX observations use at most eight concurrent requests; measure batch duration and request weights on
the actual account/venue before selecting cadence and health/market-data TTLs.
Do not relax latency limits merely to obtain a green file. A venue with enough
DEXs to exceed the reviewed latency/request budget needs a separately reviewed
observer design or cadence before launch. Full 2,000-record order/fill
histories remain refused because completeness is unknown. Rolling P&L is
`closedPnl`, not a complete account cash ledger with fees and funding.

## Configuration binding

The existing supervisor example is diagnostic-only. For a daemon, optionally
add an `operational_probe` object:

```json
{
  "executable": "/opt/hypercarry/bin/check-operations",
  "sha256": "REPLACE_WITH_SHA256",
  "timeout_ms": 2000
}
```

Compute the full supervisor policy digest offline:

```sh
hypercarry-supervisor policy-digest --config /etc/hypercarry-canary/supervisor.json
```

Put it in `runtime.supervisor_policy_digest`, then recompute runtime, gate and
release evidence digests. The optional runtime field preserves historical
digests when absent. The daemon refuses an absent or different policy digest.
Its exact executable hash and policy must also be retained in security/release
review evidence. Only one daemon can hold the health-file writer lock. Its derived lock path
is checked against state, lease, configuration and probe paths before opening.

## Operational probe integration

The supplied [operations probe](operations-probe-v1.md) verifies monitoring
receipts and actual recovery bytes. Custom probes may use the same protocol.
The probe is a **trusted, independently reviewed operations adapter**. This
repository does not manufacture alert delivery or rollback evidence. Provision
the adapter for your actual monitoring/recovery infrastructure. Its executable
hash and timeout are bound to the supervisor policy. It runs without a shell,
inherited environment, terminal, stdin, or exposed stderr, receiving:

```text
check-operations --challenge RANDOM_VALUE --integration-config-digest DIGEST
```

Within its timeout it must emit one JSON object (at most 8 KiB), exit zero,
and close stdout:

```json
{
  "schema_version": 1,
  "challenge": "EXACT_REQUEST_CHALLENGE",
  "integration_config_digest": "EXACT_REQUEST_DIGEST",
  "alerts_delivered": true,
  "alert_delivery_id": "ACTUAL_MONITOR_DELIVERY_ACKNOWLEDGEMENT",
  "rollback_ready": true,
  "rollback_artifact_sha256": "ACTUAL_RETAINED_RECOVERY_BINARY_SHA256"
}
```

The adapter must verify an authenticated, recent delivery acknowledgement for
the monitored route and verify the retained, reviewed recovery artifact and
access path. Define its acknowledgement age limit in the reviewed adapter; it
must fail on stale delivery, unavailability, wrong identity, revoked access or
changed recovery artifacts. A successful HTTP enqueue is not proof of delivery.
Check receipts rather than paging a person every observation. Test actual alerts
separately during the operational exercise. Do not implement this as a script
which prints constants: the supervisor trusts this adapter's assertions.
Missing probes, failed checks, incorrect challenges, wrong bindings, timeouts,
or malformed results cannot report readiness. Process groups are killed on
failure so child probe processes cannot survive a timeout.

The supervisor itself tests durable writes and directory synchronization in the
journal directory and validates the actual journal. It does not take the
executor's exclusive writer lock or modify its journal. Partial records fail
closed and are retried on the next observation; no journal record is repaired
or discarded automatically.

Use `deploy/mainnet/hypercarry-supervisor.service` as the service template.
The service and the probe's own filesystem/network authority are trusted;
include both in independent security review and failure-injection exercises.

The daemon and diagnostic `sample` command acquire the same exclusive health-file
lock. Sampling fails if a daemon already owns it; it cannot replace live health.
The daemon reuses one HTTP connection pool across batches. Observation, private
stream, durability and operations-probe failures emit bounded, escaped diagnostic
context on stderr while readiness remains withdrawn or not ready.
