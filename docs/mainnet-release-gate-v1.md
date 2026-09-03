# Mainnet release gate v1

M9 implements a release gate; it does not declare mainnet ready. There is no
mainnet transport or order adapter in this repository. The gate module itself
is compiled only with the default-off `mainnet-execution` feature, and the
current evidence record has one clean credentialed candidate but no independent
reviewer attestation, complete three-session set, or human release approval.

## Static evidence gate

`MainnetReleaseEvidence` is strict schema-v2 JSON. Authorization requires:

- at least three unique credentialed testnet sessions, each tied to a commit
  and an independent reviewer/final-state check, and reporting zero
  orphaned/unmanaged orders, unresolved submissions, risk bypasses, and secret
  leaks;
- a reviewed bundle that identifies the exact source commit, `Cargo.lock`
  digest, mainnet transport artifact, signer key alias, complete canary
  configuration digest, dependency audit, execution security review, and
  rollback test;
- an approving human decision made after every recorded session; and
- the decision's SHA-256 digest matching the canonical sessions and complete
  reviewed bundle, so none of them can be edited after approval.

Unknown JSON fields, duplicate sessions, invalid intervals/commits, incomplete
sessions, stale decisions, or digest mismatch fail closed.

The offline `hypercarry-testnet-operator review` command is the source boundary
for a session's independent reviewer and final-state fields. Its immutable
schema-v1 attestation binds the original harness evidence, journal replay,
official final order/open-order/fill files, executable hashes, reviewer identity,
and zero exception counts. A release preparer maps those reviewed values into
`TestnetSessionEvidence` and retains the attestation plus its SHA-256 digest in
the release audit set. The command does not edit release evidence, approve
mainnet, or prove the reviewer's organizational independence.

## Configuration and credential gate

`MainnetReleaseConfig` contains the running source/lock identities, one exact
`venue:market`, a low canary notional, the reviewed maximum canary, the
**testnet** key alias used for separation, a reviewed configuration revision,
health/latency thresholds, and a short authorization TTL. Its canonical digest
must exactly match the reviewed bundle. The canary cannot exceed the reviewed
maximum; any configuration change requires a newly approved bundle.

Each authorization consumes typed proof of explicit `--enable-mainnet` input
and exact interactive confirmation `ENABLE HYPERCARRY MAINNET CANARY`. The
signer must report network `mainnet`, a key alias different from testnet, and
the exact alias in the reviewed bundle. The gate never invokes the signer.

## Continuous operational gate

Before every future mainnet order, authorization rechecks:

- the order remains in the single market and at/below canary notional;
- startup and continuous reconciliation are ready;
- unmanaged orders and unresolved submissions are zero;
- REST and private-stream latency are within reviewed thresholds;
- the private stream, alerts, synchronized durable audit journal, and rollback
  path are ready; and
- the process-independent heartbeat dead-man control is present and fresh.

`MainnetAuthorization` has no public constructor, embeds the one exact
`ValidatedOrder`, and expires after the reviewed short TTL. A future mainnet
adapter must take the capability by value and call `into_order` immediately;
it must not accept a second caller-supplied order. It then repeats continuous
reconciliation and health gates. Limit expansion requires a newly reviewed
bundle; the gate does not infer approval from prior operation.

## Current decision

**Closed.** `docs/testnet-execution-evidence.md` records deterministic M8 tests
and one clean credentialed candidate, but that candidate has no independent
human attestation and the required three-session set is incomplete. There is no
mainnet transport, frozen release bundle, security-review record, rollback
artifact, final evidence digest, or human approval. Mainnet must not be enabled
or represented as production-ready.
