# Adversarial security review v1

Date: 2026-09-03. Scope: Z-Before-OSS.md item 3 — arithmetic/overflow/truncation
hunting across the Decimal-based financial calculations, and race-condition
(TOCTOU) hunting across the order-execution pipeline and the testnet-operator/
external-signer IPC protocol. This was a manual code review (not the `/plamen`
pipeline, which targets on-chain Solidity/Move contracts and does not fit a
Rust CLI with no deployed contract).

Method: three independent deep-dive passes — Decimal arithmetic, execution-
pipeline TOCTOU, and IPC/process-lifecycle races — each given the relevant
source files and the existing design docs as a baseline to attack, not to
restate. Every finding below was re-verified against the actual source before
being acted on. Four findings were fixed in this pass; the rest are recorded
as a scoped follow-up backlog with reasoning for why they were not folded in
here.

## Findings fixed in this pass

### [HIGH] Kill switch not re-checked before the irreversible network submit

**Location**: `crates/hypercarry-execution/src/hyperliquid.rs`, `place()`;
kill switch source `crates/hypercarry-execution/src/risk.rs`.

The filesystem kill switch (`FileKillSwitch`, explicitly documented as
"process-independent" so an operator or monitor can engage it from outside
the running process) was read exactly once, inside `policy.evaluate()`, at
the very start of `place()`. Between that read and the actual
`transport.exchange()` call, the order passes through two journal `fsync`s,
an asset-metadata resolution, and a signing round-trip — for the Foundry
keystore provider, an *interactive* step where a human must type a password,
which can take seconds to minutes. Engaging the kill switch during that
window had no effect: the order was still submitted. This defeats the one
guarantee the kill switch exists to provide ("stop everything now").

**Fix**: `RiskPolicy` gained a `kill_switch_engaged()` method (default `Ok(false)`
for policies with no kill switch, so every existing test double is unaffected).
`ComprehensiveRiskPolicy` overrides it to re-read the same `FileKillSwitch`.
`place()` now calls it immediately after signing and before the
`SubmissionPending` transition — bracketing the entire interactive signing
window instead of only preceding it. If engaged, the order is journaled and
transitioned straight to `Rejected` without ever reaching
`transport.exchange()`; no bytes cross the wire. Regression test:
`hyperliquid::tests::kill_switch_engaged_after_evaluate_aborts_before_the_irreversible_submit`,
which also asserts the scripted transport's submit queue was never touched.

The remaining gap (kill switch check → the final `transport.exchange()` call
itself) is now just the `SubmissionPending` journal fsync — milliseconds, not
the unbounded human-interactive window.

### [MEDIUM] Idempotency key derived from the raw pre-quantization intent, not the validated/submitted order

**Location**: `crates/hypercarry-execution/src/identity.rs` (`ClientOrderId::derive`),
called from `crates/hypercarry-execution/src/hyperliquid.rs`.

`ClientOrderId::derive` hashed `OrderIntent` fields — quantity and price
*before* venue-tick/step quantization — while the order that was actually
risk-checked and put on the wire was the *quantized* `ValidatedOrder`. Two
intents that round to the same submitted order (e.g. `1.29` and `1.24` under
a `0.1` size step) produced different client order IDs, so the venue-side
dedup the ID exists to provide did not apply: a resubmit after an ambiguous
failure, with a slightly different sub-tick price/size, could land as a
second live order instead of an idempotent no-op. The design doc's claim that
the ID is derived from "the complete validated intent identity" was true of
the intent, not of what was validated.

**Fix**: `ClientOrderId::derive` now takes `&ValidatedOrder` (the quantized,
risk-checked order) instead of `&OrderIntent`. The one production call site
in `place()` already had `order` in scope, so this is a same-line swap. Test
fixtures in `lifecycle.rs` and `review.rs` updated to derive from the
resolved order; `identity.rs`'s own tests now resolve through
`ValidatedOrder::resolve` before deriving, and still confirm textually-different
representations of the same rounded value (`"1.00"` vs `"1.0"`) collide.

### [LOW] `quantize_up()` panics on overflow for legal Sell-side limit prices

**Location**: `crates/hypercarry-execution/src/model.rs`.

`ValidatedOrder::resolve` quantizes a Sell limit price *up* to the venue's
price tick via unchecked `value + increment - remainder`. `OrderIntent`
validates `limit_price > 0` but imposes no upper bound, so a Sell intent with
a price near `Decimal::MAX` and a tick that doesn't evenly divide it panics
the process during order resolution — before submission, so no side effect
is corrupted, but it's a process crash on a legal input in the execution
path. (`quantize_down`, used for size and for Buy prices, can only produce a
value `<= value`, so it cannot overflow; it was not touched.)

**Fix**: `quantize_up` now returns `Result<Decimal, ExecutionError>` using
`checked_add`/`checked_sub`, propagated through `resolve()`'s existing
`Result`. Regression test:
`model::tests::resolution_fails_closed_instead_of_panicking_when_sell_quantization_overflows`.

### [MEDIUM] `basis()` panics on overflow for extreme CLI-supplied inputs

**Location**: `crates/hypercarry-core/src/metrics.rs`, reachable from the
`hypercarry basis` CLI command with raw user-supplied `Decimal` arguments.

`basis()` validated only that `spot_mid > 0` and then computed
`perp_mark - spot_mid` and `absolute / spot_mid` with the panicking operator
overloads. A `perp_mark` near `Decimal::MIN` (or a `spot_mid` in the far tail
of "positive but tiny," e.g. `1e-28`) panics the process. This is a
self-inflicted CLI robustness bug (a user can only crash their own
invocation), not a fund-affecting or attacker-vs-victim issue, but it
violates the crate's stated "exact decimal, no drift, no surprise panics"
posture. Note: a negative `perp_mark` alone is *not* a bug — the existing
property test suite (`metrics_properties.rs`) intentionally generates
negative perp marks as part of verifying basis correctly preserves sign, so
perp mark is deliberately unbounded/signed by design; only the overflow path
needed fixing.

**Fix**: `basis()`'s error type changed from the single-purpose
`NonPositiveSpot` struct to a `BasisError` enum with `NonPositiveSpot { spot_mid }`
(unchanged behavior) and a new `Overflow` variant, using `checked_sub`/`checked_div`.
This is a breaking change to a pre-1.0 (`version = "0.0.0"`), unpublished
crate, so it was made directly rather than layered around. All call sites
(`hypercarry-cli/src/basis.rs`, `metrics_examples.rs`) updated; new regression
tests cover both the overflow case and confirm negative-perp-mark basis
still computes correctly.

## Findings verified and confirmed not exploitable (no change needed)

- **Division by zero / near-zero** across `metrics.rs`, `predictor.rs`,
  `risk.rs`, `model.rs`: every divisor is guarded by a typed non-zero wrapper
  or a validated-positive constructor before use, re-validated after any
  mutation (`model.rs` re-runs `MarketMetadata::validate()` inside `resolve`).
- **Risk-limit boundary comparisons**: every limit in `risk.rs` uses the
  correct operator so the exact configured maximum is allowed and the next
  representable value is rejected (verified against `risk.rs`'s own boundary
  tests) — no off-by-one bypass.
- **Quantization direction**: Buy price and size round down, Sell price
  rounds up — conservative in every case, so quantization never lets an order
  cross a limit in the trader's favor.
- **Retry/reconcile ownership, journal lock scope, restart recovery,
  throttle/backoff windows**: the journal's exclusive OS file lock is held
  for the writer's entire lifetime; `ReconcileBeforeRetry` never blindly
  resubmits; no backoff sleep holds stale in-memory state across an `await`.
  No race found in any of these.
- **Signer one-shot phase advancement and duplicate/replay handling**: the
  Foundry signer's accept loop is single-threaded and commits its phase
  transition only after a successful write, so two connections cannot both
  obtain a signature for the same phase, and a resent request after a socket
  error hits a wrong-phase rejection rather than a second signature.
- **Evidence/journal overwrite**: both use `NamedTempFile::persist_noclobber`
  (`O_EXCL`-backed), not a separate exists-check-then-create, so there is no
  overwrite race.
- **Signal handling during interactive `cast` signing**: the documented
  `df1f911` fix (reap `cast` on interrupt, not only in the accept loop) is
  intact and was not regressed.

## Backlog — identified but deliberately not fixed in this pass

These are real, cited findings, but were left as follow-ups rather than
folded into this pass because each either needs a design decision from the
maintainer or has low real-world severity relative to the size of the change
it would require. Re-run the relevant reviewer prompt from this session's
methodology to re-verify before acting on any of these.

1. **Risk snapshot has no re-validation immediately before submit, and its
   freshness is measured against a snapshot-internal timestamp decoupled
   from the executor's own clock** (`risk.rs` `evaluate`, `hyperliquid.rs`
   `place`). Even with the kill-switch fix above, the reference price,
   account equity, and open-order count are captured once and trusted across
   the same signing-latency window. A slow or lazily-cached
   `RiskSnapshotSource` could also report `age = 0` while being stale
   relative to wall-clock time. Recommended direction: assert
   `clock.now_ms() - snapshot.observed_at_ms <= max_market_data_age_ms` at
   evaluation time, and re-run (at least) the market-data-age and kill-switch
   checks immediately before `transport.exchange` when elapsed time exceeds a
   small bound. Left open because it requires deciding the re-evaluation
   policy (full re-run vs. partial) rather than a mechanical fix.

2. **`metrics.rs`'s other unchecked arithmetic** (`funding_apr`,
   `hourly_spread`, `FundingStats::from_hourly_rates`) still uses panicking
   `+ - * /` operators on `Decimal` inputs sourced from the CLI or from API
   data. Unlike `basis()`, fixing this would change the public return type of
   several widely-used functions from a plain value to a `Result`, rippling
   into `apr.rs`, `spread.rs`, `predict.rs`, `tui.rs`, benches, and multiple
   test files. Practical exploitability is low (CLI inputs are self-inflicted
   DoS only; API-sourced values are bounded by the Parquet schema's i128
   scale check upstream of these functions). Left open pending a decision on
   whether to absorb the breaking API change now or bound input magnitude at
   the boundary instead.

3. **Operator-side socket validation checks only the immediate parent
   directory, not the full ancestor chain** (`hypercarry-testnet-operator/src/signer.rs`,
   `validate_socket`/`validate_private_parent`). If any ancestor above the
   owner-only immediate parent is writable by another local user, there is a
   check-to-`connect()` window in which the parent directory could be
   swapped. Bounded by the existing owner-only-parent requirement and
   requires a colocated local attacker plus a misconfigured ancestor;
   documented here as a known limitation of path-based checks rather than
   fixed, since the more complete fix (open the parent with
   `O_DIRECTORY|O_NOFOLLOW` and connect relative to that descriptor) is a
   larger, platform-sensitive change.

4. **A produced signature can be silently discarded when the operator's
   socket timeout fires during interactive `cast` signing**
   (`bin/foundry_signer.rs`, `signer.rs`). This is the documented `df1f911`
   failure mode's residual shape: fail-closed and no signature leak, but a
   recurring liveness trap (the operator must start an entirely new session).
   Recommended direction: either bound the interactive step out of the
   timeout window, or have the signer advance its phase only after an
   explicit application-level ack from the operator. Left open as an
   operability improvement, not a safety bug.

5. **`cast` is not placed in its own process group**, so a `SIGKILL` of the
   signer process alone can orphan an in-flight `cast` that has already
   decrypted the keystore. Residual risk is low (no temp files, password env
   vars scrubbed, `cast` dies on the next write to its now-closed stdout
   pipe), documented rather than fixed.

## Verification

`cargo test --workspace --all-features`,
`cargo clippy --workspace --all-features --all-targets -- -D warnings`, and
`cargo fmt --check` all pass after this pass's changes, including new
regression tests for all four fixed findings.
