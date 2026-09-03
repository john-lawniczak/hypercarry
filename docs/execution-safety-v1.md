# Execution safety and recovery v1

M7 adds venue-neutral safety controls to `hypercarry-execution`. It still does
not submit an order. Network submission first appears behind M8's explicit
testnet feature.

## Deterministic risk policy

`ComprehensiveRiskPolicy` evaluates one immutable `RiskSnapshot` in a fixed
order and returns a stable code plus an operator-readable reason. It rejects:

- an engaged filesystem kill switch or a market outside the exact allowlist;
- stale/future market data;
- order or projected aggregate notional above its limit;
- limit-price deviation above the configured basis-point limit;
- projected leverage above its limit;
- open-order count or inclusive rolling-window frequency at its limit; and
- rolling realized loss beyond its configured limit.

Every exact maximum is allowed; the next representable value is rejected.
Invalid limits, unavailable snapshots, time regressions, non-positive equity,
and unreadable kill-switch state are errors and fail closed.

## Signing isolation and identity

The `Signer` provider selects a typed `ExecutionNetwork` and exposes only a
bounded non-secret key alias. `validate_signer` rejects network mismatches
before signing. Hypercarry has no private-key configuration value or built-in
key loader; the provider owns secret retrieval and signing.

`ClientOrderId::derive` hashes the complete validated intent identity with
domain-separated SHA-256 and uses the first 128 bits. Its canonical form is the
Hyperliquid-compatible `0x` plus 32 lowercase hexadecimal digits. The same
durable intent always produces the same ID, including after restart.

## Reliability and recovery

`RequestThrottle` proactively enforces a monotonic rolling request window.
`RetryPolicy` bounds exponential backoff and honors a longer venue delay. A
failure after bytes may have crossed the transport boundary always returns
`ReconcileBeforeRetry`; it can never become a blind retry.

`OrderStateMachine` accepts only explicit lifecycle edges, monotonic timestamps,
a stable venue order ID, and monotonic cumulative fills bounded by order size.
Exact duplicates are idempotent. Regressive or impossible transitions fail
closed. `LifecycleTransition` records contain enough state for exact replay,
and `recover_from_journal` reconstructs one order from the append-only journal.

`FileJournal` takes an exclusive operating-system file lock for its complete
writer lifetime. A second process fails to open the journal. Existing records
and contiguous sequence numbers are validated before any append; each append
is flushed and synchronized before it is reported successful.
