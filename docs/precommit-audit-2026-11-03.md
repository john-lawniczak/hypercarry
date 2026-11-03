# Pre-commit audit — 2026-11-03

Reviewed the complete pending change set: supported crypto migration and action
codec, KMS provider, signer/watchdog integration, continuous supervisor and its
probes/reconciliation/private stream, path and atomic-write changes, offline
digest commands, dependency policy, CI and operational documentation.

**Disposition:** the implementation is suitable to commit for independent review.
No unresolved high/critical code finding was identified within this review's
scope after the fixes below. Mainnet remains closed. This is a same-author code
review supported by tests, not an independent security audit or a guarantee
that no further issue exists.

## Findings fixed before commit

| Finding | Impact and fix | Regression evidence |
|---|---|---|
| Local testnet signer serialized r/s as variable-width quantities | Valid signatures with a leading zero were rejected by the provider's required 32-byte encoding. Pad both scalars at the adapter boundary; signed values/digests are unchanged. | Deterministic fixture with a short r now passes request validation and recovers the expected identity. |
| Derived lock paths were absent from collision checks | Distinct state paths could share a lock, or a supervisor lock could alias an authority input. Include supervisor/service locks and provider config/executable in collision checks before opening/writing. | Journal, lease, source/config, provider artifact and shared state-basename cases are refused. |
| Private stream writes were unbounded | A stalled peer could keep the stream worker blocked during shutdown/invalidation. Apply a two-second deadline to subscriptions, application pings and protocol pongs. | A sink that never completes a write returns a timeout. Existing epoch/readiness tests pass. |
| Python verifier used removable assertions | Running with Python optimization could disable fixture checks. Use explicit mismatch errors and require the complete 28-case fixture set. | Official SDK conformance re-run succeeds for all 28 cases. |

## Verification

- Workspace all-feature tests: **301 passed, zero failed, four live tests ignored**.
- Strict all-target/all-feature Clippy, rustfmt and diff whitespace checks pass.
- All-feature release build, default-feature checks and Rust 1.94.1 checks pass.
- `cargo deny check`: advisories, bans, licenses and sources pass. Two unused
  license allowances produce warnings; the existing archived-paste exception is
  documented in policy. No vulnerability waiver was added.
- All 28 reference hashes/signatures agree with official Python SDK 0.24.0.
  Existing legacy Rust fixtures also match the new action codec.
- The active all-feature/all-target dependency graph has no crypto-bigint 0.5
  path. Inactive optional lockfile entries are documented in the
  [migration handoff](signing-migration-review.md); bans prevent activation.
- CLI and supervisor normal dependency graphs contain no signing stack.
- Supply-chain deny-list check passes. New signing fixtures contain explicitly
  public test keys; production configuration files remain placeholders.

Local verification is on macOS. Socket/process tests require execution outside
the restricted sandbox. No credentialed testnet session, KMS provisioning,
production deployment or live mainnet transaction was performed in this audit.
The existing public-data smoke evidence predates this follow-up; it is not a
substitute for exercising the new binaries on Linux.

Protocol checks were compared with the official
[Hyperliquid info schema](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint)
and [AWS KMS digest-signing contract](https://docs.aws.amazon.com/kms/latest/APIReference/API_Sign.html).

## Remaining release work, in order

1. Require passing Linux CI for the committed source and commission independent
   review of the exact commit, Cargo.lock, feature graph and custody/recovery
   boundary. The earlier review cannot approve this signing migration.
2. Provision the reviewed custody/account and separate executor/recovery hosts;
   implement the infrastructure-specific operational probe and protected file/
   socket transports. Pin actual artifacts and rederive configuration digests.
3. Run credentialed testnet and target-host failure exercises: stale/missing
   health, private disconnects, restarts, provider timeouts, lost replication,
   nonce replay, partial fill, bounded flattening and manual recovery. Complete
   the required three independent session reviews.
4. Freeze clean Linux artifacts, retain rollback evidence and obtain a new human
   release decision bound to the exact reviewed bundle. Only then run the single
   mainnet canary in the [launch runbook](mainnet-launch-runbook.md).

The supervisor's rolling P&L is fill `closedPnl`, excluding fees/funding; it is
not a general account cash ledger. Observation latency and venue request budgets
must be measured on the intended account. The operational probe is trusted code
whose assertions need independent review and real delivery/recovery evidence.
These existing boundaries remain explicit launch constraints.
