# Mainnet canary launch — 2026-11-03

**Decision: NO-GO for live orders until the blockers below are closed.** The
scope is one independently reviewed, minimum-size, flat-account canary. This
does not launch an unattended carry strategy or a spot hedge. No live account
was funded, key provisioned, infrastructure deployed or order submitted during
this review.

Signing-migration verification on macOS (before the later packaging/diagnostics work): **301 offline tests passed, 4 live tests ignored**;
strict workspace Clippy, format, all-feature release build, default-off checks,
and Rust 1.94.1 all-target/all-feature compilation passed. The earlier public
mainnet REST smoke test passed (not repeated for this signing migration), and the KMS public-key helper mapped a known
public secp256k1 fixture to its expected Ethereum address. The analytics CLI and
supervisor normal dependency graphs contain no signing SDK. These results are
development evidence, not a Linux deployment or live custody/recovery exercise.

For the current self-hosted preparation flow, use [the public operator guide](self-hosted-mainnet-v1.md); current verification is recorded in [development status](../DEV_STATUS.md).

The [pre-commit audit](precommit-audit-2026-11-03.md) records follow-up fixes and
verification of the complete pending diff. It does not replace independent review.

## Remaining release blockers

| Blocker | Owner and acceptance evidence |
|---|---|
| Fresh security review of the signing migration is required | Independent reviewer: review `hypercarry-signing`, custody/recovery callers and the frozen dependency graph; bind the new report to the exact source, lockfile and Linux artifacts. Earlier reviews do not cover this migration. |
| No provisioned production custody/account/hosts | Operator: actual KMS key, approved agent, dedicated account, private hosts, working transports and least-privilege access. |
| Operational probe is infrastructure-specific | Operations: integrate actual monitoring with the supplied [receipt-backed probe](operations-probe-v1.md); review its receipt writer and exercise real delivery and recovery access. A constant-output script cannot satisfy this. |
| Supervisor and custody changes lack live failure evidence | Operator/reviewer: measure real account latency, request budget, KMS response times, private reconnects, stale replication and partial-fill recovery. Offline fixtures do not establish these. |
| Three independent credentialed testnet session reviews are missing | Reviewer: finish the existing candidate review and enough additional sessions to total three, covering documented fault cases. |
| Frozen release and human approval are absent | Release owner/reviewer: freeze clean source and Linux binaries, exact policy/configuration and evidence, security review and rollback exercise, then explicit human approval. |

The signing migration now uses `k256 0.14.0`, `elliptic-curve 0.14.3` and
pinned `crypto-bigint 0.7.10`. `cargo deny check` passes with no vulnerability
waiver. The yanked 0.7.6 was not used. The old HyperSDK/Alloy signing SDK was
removed; Alloy public wire types and EIP-712 encoding remain without their
legacy crypto features. See [the migration review handoff](signing-migration-review.md)
for the feature graph, inactive lockfile entries, compatibility evidence and
mandatory fresh review. Passing dependency checks does not authorize mainnet.

## Recommended hosting and custody

Use AWS KMS **ECC_SECG_P256K1 / SIGN_VERIFY**, with a new mainnet agent key and
the account owner wallet kept offline. The KMS adapter uses `MessageType=DIGEST`
so KMS signs the existing 32-byte Hyperliquid digest; it converts DER signatures
to low-S recoverable Ethereum signatures and verifies the pinned agent address.
AWS documents the [key specification](https://docs.aws.amazon.com/kms/latest/developerguide/symm-asymm-choose-key-spec.html)
and [digest signing contract](https://docs.aws.amazon.com/kms/latest/APIReference/API_Sign.html).

Use two Linux EC2 hosts in separate availability zones with persistent encrypted
state volumes, synchronized clocks and no public service listeners:

| Host | Processes and authority |
|---|---|
| Executor host | Manual executor, credential-free supervisor, transport agent. No `kms:Sign` IAM permission. |
| Recovery host | Mainnet signer, KMS provider/AWS CLI, independent watchdog, transport agent. Instance role restricted to `kms:Sign` and `kms:GetPublicKey` on the one key ARN. |
| Independent monitoring | Delivery acknowledgement, host/service/replication health, named on-call owner with independent account access. |

Size hosts for measured workload rather than buying a trading cluster. Keep
analytics/backfill on its separate existing host. Retain a manual recovery
operator outside these hosts. Separate AZs cover an executor host/AZ failure;
both hosts still depend on AWS/KMS, connectivity, the venue and liquidity.
This layout is a recommendation, not deployed infrastructure.

The signer and watchdog share the same durable signer nonce/reservation state on
the recovery host. Do not run a second signer with independent nonce state for
the same agent. The executor reaches its owner-only signer socket over a
restricted authenticated Unix-socket forwarding tunnel. Replicate only the
allowlisted health and lease files executor → recovery, and the watchdog
heartbeat recovery → executor. Preserve heartbeat **mtime**, use atomic rename,
and restrict local files to the private service identity. Propagate STOP in both
directions as a latch: never remove it through ordinary synchronization. Do not
mirror whole state directories or replicate signer nonce files between writers.
The templates do not implement this transport; provision and fault-test it.

## 1. Provision the account, KMS key and IAM

Use a dedicated funded account with no positions or outstanding orders on any
perp DEX or spot market. Approve only the new agent address for that account.
Funding amount and canary price/quantity must be explicitly reviewed; example
prices and the example $10 order are not live recommendations. Check actual
venue minimum notional, tick/lot sizes, asset index and available collateral.

Run administrative AWS commands using your normal authenticated operator
session, not credentials embedded in the application:

```sh
aws kms create-key --region us-east-1 --key-spec ECC_SECG_P256K1 --key-usage SIGN_VERIFY --description 'Hypercarry dedicated mainnet agent'
# Copy the returned immutable key ARN; do not use a mutable alias in config.
aws kms get-public-key --region us-east-1 --key-id '<KEY_ARN>' --output json > /private/canary/kms-public.json
hypercarry-kms-provider --public-key-json /private/canary/kms-public.json
```

The last command derives the agent Ethereum address offline. Verify it, approve
it through the account owner's normal venue flow, and put its lowercase address
in the runtime and KMS configuration. Retain the public key response. Never
provide a master wallet key to the service. Document agent revocation and who
can perform manual account recovery if KMS is unavailable.

Install a pinned AWS CLI v2 distribution on the recovery host. Resolve the real
AWS executable path (no symlink), retain its distribution version and SHA-256,
and make the installation root-owned and not group/world-writable. The adapter
clears inherited environment, disables shared AWS config/credentials files,
disables IMDSv1 and retries, and uses the EC2 instance role through IMDSv2.
No AWS access key or secret belongs in JSON, environment or evidence. This
adapter intentionally does not support local SSO/profile or ECS credentials.

Copy [kms-provider-config.example.json](kms-provider-config.example.json)
outside Git and replace all placeholders. Set the service policy backend to
`/opt/hypercarry/bin/hypercarry-kms-provider`, with its executable hash, and add:

```json
{
  "backend_config": "/etc/hypercarry-canary/kms.json",
  "backend_config_sha256": "SHA256_OF_EXACT_KMS_CONFIG_FILE"
}
```

The signer hashes this configuration before every provider invocation, then
passes `--config PATH`; its identity is included in the reviewed service-policy
digest. Test KMS access denial, disabled key, wrong agent, wrong key ARN,
timeouts and malformed output before review. Bound the KMS timeout below the
backend timeout, and allow the signer timeout/action TTL for the **whole**
agent lookup, readiness checks and KMS round trip. Never increase thresholds
without measuring and reviewing them.

## 2. Install, observe and exercise recovery

Create a private service OS identity on both hosts. Use canonical 0700 config
and state directories, 0600 configs/evidence, root-owned reviewed executables,
persistent signer/watchdog/journal state and correct clocks. Adapt the units in
`deploy/mainnet/`; they are not automatically installed or started.

Integrate the supplied [operations probe](operations-probe-v1.md) with actual
monitoring delivery/recovery receipts.
Freeze its executable and any configuration it consults. Its identity and
policy must cover the monitored alert destination, receipt freshness and exact
retained recovery artifact. Readiness cannot be obtained by editing health JSON.

Exercise these cases on the chosen topology and retain secret-free observations:

1. Executor killed or host disconnected: lease expires, heartbeat withdraws,
   STOP latches, entry cancels, bounded reduce-only IOC runs if needed, and
   fresh queries prove zero orders **and** zero perp positions across all DEXs.
2. Partial fill/cancel race, signer outage, HTTP 429, exhausted price collar,
   private-stream disconnect, and stale copied lease/heartbeat/health files.
3. Recovery host unavailable: the already-armed venue cancellation survives,
   independent alert reaches the operator, and the operator handles any fills.
4. Reboot/restart preserves consumed nonce, one-placement reservation, journal,
   and emergency state. No automatic retry or state deletion is permitted.

Confirm `userRateLimit` capacity, IP request weights, and the re-arm cadence for
the full session plus emergency actions. A venue scheduled cancel removes open
orders; it cannot flatten an existing position. Venue timing and trigger limits
are documented in [schedule cancel](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint#schedule-cancel-dead-mans-switch).

## 3. Freeze and review a release

Complete [testnet session reviews](testnet-session-review-runbook.md) and the
execution/security/rollback reviews, including a fresh independent review of
the signing migration. Save a passing dependency audit for the frozen graph.
Commit the final reviewed source, then use a **clean Linux checkout** on the
deployment architecture. Run:

```sh
tools/check-rust-supply-chain.sh
cargo deny check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo +1.94.1 check --workspace --all-targets --all-features --locked
cargo build --workspace --all-features --release --locked
git status --porcelain
sha256sum Cargo.lock target/release/hypercarry-executor target/release/hypercarry-mainnet-signer target/release/hypercarry-watchdog target/release/hypercarry-supervisor target/release/hypercarry-kms-provider
```

Do not use this review's dirty-tree/macOS binaries for deployment. Store all
deployment configs, evidence and generated hashes **outside the checkout**.
Keep the exact Linux binaries available for recovery. If you commit again,
rebuild and reapprove the resulting source/binary identities.

Bind configuration in this order to avoid circular approvals:

1. Exact binaries, AWS CLI and KMS configuration → service policy digest.
2. Supervisor settings and operational probe identity → supervisor policy digest.
3. Put both in `runtime.services_policy_digest` and
   `runtime.supervisor_policy_digest`; set `runtime.executor_lease` to the
   service policy path. Finalize the exact order, paths, limits and timers.
4. Runtime digest → gate `integration_config_digest`; gate review digest →
   evidence bundle `config_digest`.
5. Three reviewed testnet sessions, source/lock/binary identities, security,
   dependency and rollback records → evidence digest → human approval.

Use the read-only preparation commands (they do not approve anything):

```sh
hypercarry-mainnet-signer --policy /etc/hypercarry-canary/services.json --policy-digest
hypercarry-supervisor policy-digest --config /etc/hypercarry-canary/supervisor.json
hypercarry-executor config-digest --runtime /private/canary/runtime.json
# After setting gate.integration_config_digest to the runtime digest:
hypercarry-executor gate-digest --config /etc/hypercarry-canary/config.json
# After setting reviewed_bundle.config_digest to the gate digest:
hypercarry-executor evidence-digest --evidence /etc/hypercarry-canary/release.json
```

Only the approving human sets the final decision, named approver, timestamp and
returned evidence digest. `evidence-digest` deliberately works on unapproved
drafts; `preflight` still refuses incomplete or unapproved evidence.
Further schema-v2 evidence requirements are in
[mainnet-integration-v1](mainnet-integration-v1.md) and
[mainnet-release-gate-v1](mainnet-release-gate-v1.md). No approving evidence is
generated by this review. Run static preflight with the frozen binary:

```sh
hypercarry-executor preflight --config /etc/hypercarry-canary/config.json --evidence /etc/hypercarry-canary/release.json
```

## 4. Run the one approved canary

Only after every blocker is closed: start protected transports and the recovery
signer/watchdog, then the executor-host supervisor. Confirm fresh genuine
readiness. The watchdog waits for the executor's lease before arming; the
executor starts that lease and waits for an acknowledged protection heartbeat.
Never touch a heartbeat or manually generate a lease to bypass this handshake.
Clear an existing STOP only after its incident is reconciled and the release
owner has explicitly authorized restarting.

From an interactive terminal on the executor host:

```sh
hypercarry-executor run --config /etc/hypercarry-canary/config.json --evidence /etc/hypercarry-canary/release.json --enable-mainnet
```

Read the exact order and type `ENABLE HYPERCARRY MAINNET CANARY` only for that
reviewed release. Use the interactive path for the first canary; MCP adds an
optional consent/transport layer but does not remove any gate.

After the executor exits, lease withdrawal causes watchdog recovery even after
a filled order. Keep watching until fresh venue queries establish no orders and
no perp positions on **every** DEX. Retain the final order/fills, position
snapshots, journal, service states and reviewer sign-off. A zero executor exit
code or canceled order alone is not a flat-account result.

For incidents, latch STOP and preserve state. Do not run executor `recover`
concurrently with a live watchdog. Prefer the independent watchdog's explicit
`--recover` after stopping its normal process; it can cancel and attempt bounded
flattening. Executor `recover` reconciles/cancels orders but does not itself
flatten positions. If automated recovery cannot prove flatness, use the
prearranged account owner/operator path and keep the incident open.

Do not delete a journal, nonce file or placement reservation to retry. A second
canary requires a new reviewed release and explicit state/identity rollover.
