# Testnet execution evidence

This file is intentionally an evidence record, not a declaration of readiness.
Build scopes and crate paths in dated entries refer to the recorded revision.
The venue adapter moved to `hypercarry-hyperliquid` on 2026-09-06; this migration
does not retroactively change or approve any historical evidence.

## Deterministic M8 evidence — 2026-08-26

- Build scope: `hypercarry-execution` with `testnet-execution`.
- Result: 37 unit tests and doc tests passed; strict Clippy passed.
- Covered: allowed/rejected placement, resting acknowledgement, partial fill,
  duplicate fill, cancel/fill race, cancellation, REST reconciliation,
  uncertain submission, restart replay, malformed private data, throttling,
  stale quotes, risk boundaries, and kill-switch behavior.
- External effects: none. Scripted signer/REST providers were used.

## Deterministic signing evidence — 2026-08-27

- Build scope: `hypercarry-execution` with all features, including the pinned
  `hypersdk` 0.2.15 signer adapter.
- Result: 47 execution tests passed; strict Clippy, dependency policy, and the
  full 148-test workspace suite passed.
- Covered: exact order/cancel action, nonce, expiry, and null-vault preservation;
  signature recovery to the selected signer; rejection of provider mutation,
  vault injection, and unpinned signer identity.
- Public live checks: mainnet/testnet read-only REST and testnet WebSocket smoke
  tests passed. No signed request or order was sent.
- External effects: public reads only. The signing fixture key is deterministic
  test data and controls no recorded account.

## Credentialed evidence is private

Retain at least three independently reviewed sessions privately, binding exact
source and binaries, UTC interval, account, allowlist/limits, injected faults,
final lifecycle counts, independent positions/open orders, unresolved submissions,
policy-bypass count, secret-scan result and named human reviewer. Keep immutable
artifacts and their hashes. Public fixture results are not credentialed evidence.

The offline `hypercarry-testnet-operator review` workflow verifies recorded hashes,
replays the journal and checks retained official terminal/open-order/fill artifacts.
It creates a separate non-overwriting reviewer attestation. The human must perform
the independent venue checks and manual secret scan; software cannot assert them
on the reviewer's behalf. No operator-specific session record is published here.

## Deterministic operator harness evidence — 2026-08-30

- Build scope: non-published `hypercarry-testnet-operator` plus the
  `testnet-execution` library feature.
- Covered: typed testnet-only strict configuration, rejected unknown/secret
  fields, stale/future snapshot failure, pinned external-signer response
  identity, owner-only socket directory policy, bounded response parsing, and
  atomic non-overwriting evidence.
- Runtime boundary: the harness can issue one place/cancel/REST-reconcile flow
  only after the exact external-effects acknowledgement and all normal risk,
  journal, throttle, signer, and kill-switch gates pass.
- External effects: none during this verification. No wallet was created, no
  credentials were available, and no signed request was sent.

The harness creates candidate evidence only. Its schema leaves manual secret
scan, reviewer, and injected-fault records incomplete, so an unreviewed harness
run cannot satisfy the three-session release gate. The offline reviewer command
now provides the immutable attestation boundary documented in
`docs/testnet-operator-v1.md`; it does not edit the harness evidence or replace
the required human review.
