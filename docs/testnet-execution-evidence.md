# Testnet execution evidence

This file is intentionally an evidence record, not a declaration of readiness.

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

## Credentialed live sessions

### Clean candidate — 2026-09-01

- Session: `testnet-btc-20260901T160209Z-f8a0d9b9`, commit `34644b0`, from
  2026-09-01T16:03:57Z through 2026-09-01T16:04:23Z.
- Exact action: buy `0.00015` BTC at `78738` on official testnet; notional
  `11.8107` USDC, client ID `0x6f09df9c73c077a5f3c31f53fc906176`, venue
  order `59071289661`.
- Harness result: `cancelled`, cumulative fill `0.00000`, zero unresolved
  submissions, zero policy bypasses, and zero independent open orders.
- Separate final-state check: official status `canceled` by venue order ID,
  zero account-wide open orders, and zero matching user fills.
- Automated artifact scan found no private-key, seed, mnemonic, password,
  signature, or 32-byte `0x` secret pattern. The session directory and every
  retained artifact are owner-only.
- SHA-256: config `8b17a5527a006eda7b9d55f95765d6f6be145b39e18b4e190a4e6e8da3a84ddf`;
  journal `e5700c6389fdeb15d6dc32b605610ce1fe8cd06ff5b0b43e65e9e5ca98c5dbad`;
  evidence `ac5819af8dd09f588b0d3af0edbae99f56695ebebe24fd9d31265a08f28bca2c`;
  operator `e959c10f38fc2fb30d9eccb21d4843296dfa3d6cdf9d0c9e5ba4f1be161589ec`;
  signer `6cad3eb598a14d886bb9a8822771cac415ae78242e465fd8c2881bc175fbed67`;
  independent order status `2d87bb4b1bcb8b61396b6957ed289dba5b6d35b29158998617b6e458e0b9ca21`;
  manifest `a76707f7c1d159bd260299269ddd5273453787158fed74c41554d08e911a21de`.
- Release-gate status: candidate only. The immutable harness evidence retains
  `manual_secret_scan_completed: false`, `reviewer: null`, and no injected
  faults. It cannot count toward the required three sessions until an
  independent human reviewer manually scans the artifacts and runs the offline
  `hypercarry-testnet-operator review` workflow. That command verifies the
  recorded hashes, replays the journal, checks the official terminal/open-order/
  fill artifacts, and creates a separate non-overwriting reviewer attestation.
  No attestation has been issued for this candidate yet.

Before M9 can open, record at least three independently reviewed sessions with
commit, UTC interval, account, allowlist/limits, faults injected, final lifecycle
counts, independent open-order count, unresolved submission count,
policy-bypass count, secret scan result, reviewer, and secret-free artifact IDs.

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
