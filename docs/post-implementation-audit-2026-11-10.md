# Post-implementation audit — 2026-11-10

Scope: account/bracket foundations, notifications and stop-failure recovery,
including nearby transport, signer validation, journal and release boundaries.
No real key was unlocked, trading action submitted or provider message sent
during this implementation review. Private operational records are excluded.

**Ship rating: 6/10 overall.** Appropriate to merge as explicitly gated local
research/testnet components after these fixes; not ready to enable unattended
mainnet trading. This is a post-implementation review with regression testing,
not independent live-release evidence or a financial-loss guarantee.

## Fixed findings

### 1. P1 — A triggered stop could block the very fallback intended to handle it

Location: `crates/hypercarry-hyperliquid/src/venue/recovery.rs`, `snapshot` exit
reconciliation. `triggered` was classified as open, then required to appear in
`frontendOpenOrders`. When the trigger had left that list with residual exposure,
the adapter returned an error before delivering `stop_fired` or the position to
the driver. Repeated errors led to deadline escalation instead of a bounded close.

Fix: accept absence specifically for a triggered owned exit, preserve the fired
signal, and read residual exposure. Ordinary open/missing inconsistencies and
unknown child orders still block writes. The regression verifies both refusal of
an inconsistent open exit and a signed reduce-only close after a triggered exit
leaves the list; it never equates triggering with filling.

### 2. P1 — Known terminal venue outcomes could strand an uncertain close

Location: the same adapter's `order_evidence`. Documented statuses including
`marketOrderNoLiquidityRejected`, `oracleRejected`, and `perpMaxPositionRejected`
were mapped to Unknown. After an ambiguous submission, such an outcome could
leave the pending reservation unresolved until escalation despite definitive
venue evidence that it was rejected.

Fix: enumerate the documented terminal cancellation/rejection statuses. Keep
unrecognized future statuses Unknown. Tests exercise every newly supported
status, wrong identifiers, and an unrecognized status. The supported vocabulary
is taken from the [official order-status API](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint).

### 3. P1 — Signing authority and book freshness could expire during reconciliation

Location: adapter `snapshot`/`send_checked`. Agent approval was checked against
snapshot-start plus TTL, rather than the actual later action expiry. Book age
was checked relative to snapshot start, separately from snapshot age; their two
age allowances could accumulate. An emergency order could consequently reach
signing with expired authority or a stale executable quote.

Fix: retain the approval deadline and require it to cover the exact reservation
expiry. Determine authorization at snapshot completion. Retain the venue book
timestamp and recheck its age at completion, before signing and after signing.
Also reject a regressed clock across signing. Regressions move time between the
snapshot and submission and verify no exchange write or nonce reservation occurs
when the required approval/book lifetime is no longer available.

### 4. P2 — The final write boundary did not enforce relative slippage

Location: adapter `recovery_wire` and execution driver's price rounding. The
adapter checked the absolute collar but trusted its caller on the tighter
relative slippage limit. Separately, a coarse tick could round an IOC price away
from the book, wasting the finite close-attempt budget without a marketable price.

Fix: independently enforce the observed bid/ask, relative slippage and
marketability before signing. Escalate without spending an attempt when inward
rounding leaves no executable price inside the policy. Tests reject both an
excessively aggressive price inside the absolute collar and a nonmarketable price,
and verify zero attempts for the coarse-tick case.

### 5. P1 — One broken notification source stopped unrelated alert delivery

Location: notification `Outbox::open`, ingestion and worker loop. Missing sources
failed canonicalization at open; malformed or changed sources failed ingestion
before the worker reached delivery. A single journal error could therefore stop
already-durable critical alerts and healthy source ingestion.

Fix: opening the queue no longer depends on source availability. Import each
source transactionally, retaining failed cursors and reporting only configured
source IDs. Continue healthy-source ingestion and drain existing work. Continuous
mode retries degraded sources; once mode drains then returns nonzero. The restart
regression removes one source, delivers both existing and healthy-source events,
then repairs the source without duplicating sends. State-file aliases remain
rejected, and filesystem/persistence failures still stop the worker.

### 6. P2 — Slow delivery calls used a stale clock for the rest of the pass

Location: `Outbox::tick`. Up to 32 provider operations used one initial timestamp.
Later events could be sent after their age limit, and retry delays were measured
from request start rather than response completion.

Fix: add a clock-driven pass used by the production worker, refreshing time
around provider I/O and before each delivery decision. Keep the fixed-time helper
for deterministic callers. A simulated slow provider verifies that a subsequent
expired event is not sent and that backoff starts when the response completes.

### 7. P2 — Reminder bookkeeping could prevent a full outbox from draining

Location: `add_delivery` and reminder scheduling. Capacity was checked before
checking for an existing reminder, and scheduling a new reminder at capacity
aborted the entire pass. Queued originals could stop sending precisely when the
queue was full. Acknowledged queued reminders also remained Queued indefinitely.

Fix: deduplicate before capacity enforcement; defer new reminders at capacity
while draining existing work. Mark acknowledged queued reminders Superseded.
Tests seed a full 10,000-delivery outbox and verify an original still sends, and
verify acknowledgement removes a rate-limited reminder from the active queue.
Explicit archival/migration is still required to ingest beyond retention limits.

### 8. P2 — A failed recovery session looked successful to a supervisor

Location: `hypercarry-testnet-operator/src/bin/recovery.rs`. Escalation and session
termination with unresolved exposure printed a warning but returned exit code 0.
A local supervisor could misclassify this as a successful recovery.

Fix: success requires Closed, or Monitoring for an explicit single pass. Other
outcomes print their state and return an error. Tests cover all recovery phases
and both single-pass and bounded-session modes. Exit status does not replace
inspection of the exposure and pending reservation.

### 9. P2 — A blocking failed read delayed incident-deadline enforcement

Location: `RecoveryManager::tick`. The read-error branch persisted the timestamp
sampled before the network operation. A slow failure crossing the incident
deadline could leave the driver reconciling until another poll.

Fix: resample and validate time after the failed read. A delayed-read regression
verifies escalation in the same pass without any venue action.

### 10. P2 — Private-file checks and opens operated on different path lookups

Location: notification `read_private`/`lock_private`, also used by the recovery
host. Checking metadata before an ordinary open allowed a path replacement
between validation and use. The opened file was not itself validated.

Fix on Unix: use safe `rustix` descriptor opens with `O_NOFOLLOW`, `O_CLOEXEC` and
`O_NONBLOCK`, then validate the opened inode's type, permissions, link count and
size. The existing locked dependency version is reused; there is no key-loading
or signing dependency. Tests reject symlinks, hardlinks, permissive files and
FIFOs without waiting for a writer. All ancestor directories must still be
operator-controlled; this is not protection against compromise of the local user.

### 11. P3 — Lifecycle market labels exceeded the downstream alert contract

Location: `BracketSpec::validate`. Lifecycle initialization accepted an unbounded
alphanumeric market name, while the alert contract permits at most 32 bytes.
Such a journal could later produce alerts that ingestion rejected.

Fix: validate the shared label bound before creating the lifecycle. A regression
rejects a 33-byte market name. Supported native-market scope is unchanged.

## Remaining risks and exact next work

1. **Mainnet workflow integration is still incomplete (release blocker).** The
   new host is testnet-only; the old Foundry canary signer does not authorize the
   new bracket/recovery workflow. Implement a separately scoped local agent
   signer and entry/protection assembly, bind the full reviewed plan at the
   signing boundary, and prove entry, partial fill, native stop, fallback,
   restart and revocation using a dedicated credentialed testnet account. Do not
   widen the old signer as a shortcut or manually fabricate journal ownership.
2. **Host outages and latency can leave residual exposure (release blocker for
   unattended operation).** Recovery and notifications share one local failure
   domain. The host polls at a minimum of 15 seconds and performs sequential REST
   reads; there is no WebSocket wake-up or independent heartbeat service. Add a
   reconnect-safe event wake-up path, measured end-to-end deadline budgets, and
   independently hosted missed-heartbeat alerting. Venue outages, price gaps,
   insufficient liquidity and revoked agents still cannot guarantee closure.
3. **Production API/provider behavior remains unproven.** Tests exercise signed
   fixture requests, documented statuses and simulated delivery. A triggered
   order's child linkage, cancellation races and real partial-fill outcomes need
   recorded credentialed contract tests. Provider tests lack HTTP-level timeout,
   malformed-body, rate-limit and receipt integration coverage. Add controlled
   HTTP fault tests and an explicitly authorized delivery smoke test before
   relying on email/SMS operationally. Unknown evidence continues to fail closed.
4. **Outbox storage has a concrete scaling cost.** Each poll reads and hashes whole
   source prefixes; each transition clones/rewrites/syncs the JSON state. At the
   existing 32 MiB/10,000-delivery bounds this can delay delivery and block local
   status/ack requests behind the worker lock. Benchmark at the bounds, then use
   a transactional store with indexed deliveries, incremental source cursors and
   explicit archival migrations. Do not discard cursors to make performance look
   better: that can duplicate old sends.
5. **Local persistence is not a tamper-proof ledger.** Recovery replay validates
   identity, time ordering and budgets, but does not replay every checkpoint as
   an independently verified transition. Parent-directory ownership is an
   operational assumption, and no power-loss test proves filesystem durability.
   Before mainnet release, strengthen checkpoint invariants, retain external
   audit evidence, and exercise process/power-loss recovery. Do not claim that a
   private JSON file is authenticated merely because it has mode 0600.

The top three highest-leverage improvements are: (1) scoped signer plus complete
credentialed bracket/failure proof; (2) event-driven recovery with independent
heartbeat monitoring; (3) transactional outbox storage and provider fault testing.
These are larger integration tasks, not safe incidental refactors in this audit.

## Verification

Final check results are recorded in the accompanying `DEV_STATUS.md` audit entry.
The reproducible check set is:

```sh
cargo test --workspace --all-features --offline -- --test-threads=1
cargo clippy --workspace --all-targets --all-features --offline -- -D warnings
cargo +1.94.1 check --workspace --all-targets --all-features --offline
cargo fmt --all --check
python3 -m unittest discover -s tools/tests -p 'test_*.py'
uv run --offline --no-project --with hyperliquid-python-sdk==0.24.0 python tools/verify-signing-vectors.py
cargo deny --offline check --disable-fetch
bash tools/check-rust-supply-chain.sh
python3 tools/check_public_export.py
```

Regression coverage uses fixture keys, scripted exchange responses and fake
notification senders. Opt-in credentialed live tests remain ignored. The audit
commit is intended only for the private remote; no public push or deployment is
part of this review.
