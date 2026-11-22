# Local stop-failure recovery

A stop price is a trigger, not a guaranteed execution price. Native exits are the
first protection layer. This implementation adds a durable fallback loop for an
**owned, journaled bracket**. It does not adopt manually opened positions and
never uses account-wide cancellation to remove a held position's protection.

## What is implemented

- `execution::recovery`: durable policy/state/action reservations and a bounded
  driver. It freezes new entries before every pass, requests fresh REST evidence,
  and permits at most one external action per pass. Stream events may wake a
  host, but cannot authorize an action without REST reconciliation.
- `TestnetRecoveryAdapter`: actual Hyperliquid account/status/book/metadata/agent
  queries, scoped cancel-by-client-ID and signed reduce-only IOC closes. Its only
  constructor is testnet. It validates signed content and recovered signer,
  uses a shared account lock, persists nonces before signing, and closes a local
  entry gate. All entry processes must honor that gate and use the same shared
  operations directory; a file cannot stop a separate manual wallet or UI.
- `hypercarry-testnet-recovery`: a local bounded-session polling host. It resumes
  a recovery journal, pins its original bracket specification and limits, and
  talks to a separately started private signer socket. It loads no wallet key.
- The [local agent integration](local-operator-integration.md) derives a recovery
  policy from journal replay, preserves repaired exit IDs separately from the
  original specification, and limits signing to exact owned client IDs and
  bounded reduce-only closes. Large DEX sets use complete fresh subscriptions
  with target REST checks; the host remains a bounded polling process.
- Bracket and recovery transitions atomically include optional notification
  events. Entry fills, funding observations, protection changes, uncertainty,
  revocation, recovery failure and confirmed closure feed the separate
  [notification worker](notifications-v1.md). Delivery cannot block trading.

No mainnet recovery service is enabled. The existing mainnet canary release
checks are unchanged. This adds working driver, adapter and host code, not
credentialed deployment evidence or an expanded mainnet authorization.

## Decision sequence

1. Close the entry gate and fetch scoped, recent evidence. Resolve the original
   entry by client ID. Unknown status is not proof of absence. Pending entry
   orders are canceled individually and reconciled before any fallback close.
2. Monitor the actual signed position, mark price and owned native TP/SL orders.
   A stop crossing/firing with residual exposure or missing/mismatched protection
   starts a persistent incident timer. A configured grace interval allows native
   execution to finish. A transient unknown-state incident also freezes entries;
   once evidence returns, the conservative policy closes residual exposure.
3. Once the entry is terminal, close only the freshly observed residual, using
   the opposite side, `reduceOnly: true`, `tif: Ioc`, inward price rounding,
   an explicit slippage bound and a fixed reviewed absolute price collar.
   For a short, closing means **buying**, not selling again.
4. Persist the exact action, client ID, nonce, expiry and consumed attempt before
   signing/submission. The HTTP adapter has no implicit write retries. An
   accepted response does not prove a fill. A transport timeout retains the
   original reservation across restart; do not submit another close until the
   original client ID has authoritative terminal evidence.
5. Re-read the position after each terminal outcome. Example: an original 1.1
   unit short closes 0.7 units; the next bounded order is for the remaining 0.4.
   Reduce-only also prevents a native stop racing the fallback from opening a
   reverse position. A definitive rejection can retry after a new snapshot and
   the configured delay; uncertain writes cannot.
6. Keep native exits while exposed. Once flat and the entry/last close are
   reconciled, cancel only the owned orphan exit IDs, then confirm flatness and
   absence again. Leave the entry gate closed even after successful cleanup.

Agent revocation, foreign orders, unexpected direction, increased position
size after entry termination, unquantized dust, price outside the collar,
exhausted attempts or the incident deadline cause a sticky escalation. New
entries stay frozen. Fresh evidence can confirm a subsequently flat/clean
account, but cannot restore signing authority or reset the retry budget.
Read outages also escalate at the persistent deadline. No loop increases the
price collar indefinitely or declares success merely because retries stopped.

Policy limits are finite: up to eight close and eight cancel attempts; 1–10
second action TTL; 5–300 second incident timeout; at most 1% fallback slippage.
The submitted price must also fit the absolute collar and live venue precision.
The adapter independently enforces the relative slippage collar and verifies
that the price can cross the observed book. If tick rounding leaves no marketable
price within approved bounds, the driver escalates without consuming a close
attempt. Agent approval must cover the actual action expiry; book timestamps are
checked again before signing and after signing. Triggered exits absent from the
open-order list are not mistaken for fills: residual position evidence still
controls recovery, and unknown child orders still block automated writes.
There is still **no execution guarantee** through a gap, empty order book,
revoked signer, venue outage or host/network failure. Escalation is a documented
residual-risk outcome, not a claim that the position has closed.

## Local host setup

Build the host and use a private configuration outside Git:

```sh
cargo build -p hypercarry-testnet-operator --bin hypercarry-testnet-recovery --release
```

Example structure (all paths and the specification digest must be replaced):

```json
{
  "schema_version": 1,
  "network": "testnet",
  "bracket_journal": "/absolute/private/owned-bracket.jsonl",
  "bracket_spec_digest": "0000000000000000000000000000000000000000000000000000000000000000",
  "operations_dir": "/absolute/private/operations",
  "asset_id": 0,
  "signer_address": "0x3434343434343434343434343434343434343434",
  "signer_alias": "testnet/recovery-agent",
  "signer_socket": "/absolute/private/recovery-signer.sock",
  "signer_timeout_ms": 1000,
  "poll_ms": 15000,
  "session_ms": 900000,
  "limits": {
    "size_step": "0.01", "price_tick": "0.01",
    "min_price": "100", "max_price": "200", "slippage_bps": 50,
    "stop_grace_ms": 0, "max_snapshot_age_ms": 10000,
    "max_close_attempts": 3, "max_cancel_attempts": 4,
    "retry_ms": 15000, "recovery_timeout_ms": 180000,
    "action_ttl_ms": 5000
  }
}
```

These are illustrative bounds, not a recommended trade. Pin the digest of the
actual `BracketSpec`, not just the preview digest. The journal must already have
an owned entry reservation; do not fabricate one to adopt a UI position. The
host holds the bracket journal lock, so transfer lifecycle ownership to recovery
before starting it. Do not run a competing entry executor for the account.

The signer must implement the existing private Unix-socket signing protocol and
independently restrict account/agent/network/asset, reduce-only close purposes,
owned cancel IDs, size, prices, nonce and expiry. **The old one-order Foundry
canary signer does not authorize this workflow.** Use the implemented scoped
local agent signer and `plan-recovery` handoff described in
[the integration guide](local-operator-integration.md). Credentialed end-to-end
scenarios and independent review remain deployment prerequisites. Unlock the
bounded signer session before starting recovery; an attended local process does
not establish host-independent emergency availability.

```sh
hypercarry-testnet-recovery \
  --config /private/recovery.json \
  --acknowledgement 'I ACKNOWLEDGE HYPERCARRY TESTNET EXECUTION AND EXTERNAL EFFECTS' \
  --once
```

Without `--once`, the host polls for a bounded session and exits on closure,
escalation, interruption or session expiry. The 15-second minimum polling
interval is conservative for the adapter's complete scope and REST checks.
Large discovery sets use fresh stream bootstrap on each pass, not a persistent
stream cache. Review aggregate API usage before running
other clients. There is no WebSocket wake-up wiring in this host yet. Polling
latency, grace, signer delay and venue latency all contribute to response time.
Exit code 0 means verified closure, or a healthy `Monitoring` observation with
`--once`. Escalation, an unresolved single pass, or session termination without
closure returns nonzero after printing state. Supervisors must inspect that
state and retained journal; an exit code alone does not describe the exposure.

The private operations directory contains:

- `testnet-ACCOUNT.lock`: exclusive account recovery writer lock.
- `testnet-ACCOUNT.nonces.json`: durable nonce binding to the agent and account.
- `testnet-ACCOUNT.entries-disabled`: gate honored by the local Python entry
  path and consumed by `FileKillSwitch` in Rust entry risk checks. Do not remove automatically on restart or successful close.
- `testnet-ACCOUNT-TRADE.recovery.jsonl`: transition/reservation/alert journal.

Use mode 0700 for the directory and 0600 for configuration and journal files.
Point the notification worker's source at the recovery and/or bracket journal.
They retain separate deterministic event identities; configure sources/routes
intentionally to avoid duplicate human alerts from two overlapping observers.
Unchanged polling observations do not append identical checkpoints. Notification
source size/retention limits still require explicit archival for long sessions.

A new nonce file is not permission to rotate agents or discard unresolved
orders. Policy drift is refused. After escalation, inspect authoritative state
and the retained pending request; do not delete the journal to reset budgets.
SIGINT/SIGTERM stop the host with the gate closed and native exits retained.
Local notification and recovery processes share the same host failure domain;
independent remote heartbeat monitoring remains an optional separate deployment.

## Verification and remaining release work

Deterministic tests cover partial fills, native-stop races, uncertain writes,
restart/disk replay, pending-entry cancellation, stale/wrong-scope evidence,
revocation, unexpected exposure, persistent grace/deadline/attempt caps, dust,
long/short direction, orphan cleanup, journal failure and notification import.
Adapter tests use a deliberately public fixture key and scripted transports;
no real wallet is unlocked, exchange request sent or notification delivered.

Before live mainnet use: verify credentialed bracket/recovery scenarios and
API response contracts through the implemented scoped signer and handoff,
add independent operational/release evidence, and review the local mainnet
profile. The existing mainnet watchdog remains scoped to its old canary and is
not used as a generic account-wide canceller for protected held positions.

Sources: [Hyperliquid TP/SL](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/take-profit-and-stop-loss-orders-tp-sl),
[info API](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint),
[exchange API](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint).
