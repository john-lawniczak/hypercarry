# Self-hosted mainnet preparation

The supported execution scope is one reviewed canary on a dedicated, initially
flat account, with independent recovery. Analytics can run immediately against
public data. Automated carry selection, spot hedging and unattended entry loops
are not implemented. The execution gate remains closed until your deployment has
its own reviewed evidence and approval.

This guide is the public entry point for a new operator. Example addresses,
hashes, order sizes, prices and evidence are placeholders. The repository's
historical session candidate is not evidence for your account or infrastructure.
Do not distribute private configuration, credentials or account evidence with
the software.

## 1. Establish the read-only baseline

Use Rust 1.94.1 or newer and a clean checkout of the source you intend to review.
These commands need no wallet, signing service or AWS credentials:

```sh
cargo run -p hypercarry-cli -- snapshot --network mainnet --coin BTC
cargo run -p hypercarry-cli -- snapshot --network testnet --coin BTC
cargo run -p hypercarry-core --example read_market -- mainnet
cargo run -p hypercarry-core --example read_market -- testnet
```

The Rust example uses the typed public info client, requires an explicit network,
checks metadata/context alignment and emits decimal strings. It observes the
default perp universe; it is not the all-DEX account reconciliation used by the
execution supervisor. Tests compile it without making network requests. Account
custody and execution are separate opt-in features.

For continuous funding data, follow [the recording deployment](../deploy/README.md).
Validate storage persistence, clock synchronization, freshness and restart behavior
before relying on it. Optional L2 recording does not replace settled-funding backfill.

## 2. Prepare hosts and custody

Use the passive [AWS reference template and validation workflow](cloud-validation-v1.md)
to prepare your topology, then freeze the exact software/configuration and retain
independent observations with the [release dossier](release-review-dossier-v1.md).

AWS KMS is the reference backend. Use a dedicated agent key with
`ECC_SECG_P256K1`, `SIGN_VERIFY`, and an immutable key ARN. These match the
[AWS key specification](https://docs.aws.amazon.com/kms/latest/developerguide/symm-asymm-choose-key-spec.html)
and [digest-signing contract](https://docs.aws.amazon.com/kms/latest/APIReference/API_Sign.html).
Keep the owner wallet
outside the application; approve the agent through the normal venue owner flow.
The [launch runbook](mainnet-launch-runbook.md) contains KMS commands and links to
the AWS signing contract. Other providers must implement and be reviewed against
[the external provider protocol](mainnet-services-v1.md).

| Placement | Processes and permissions |
|---|---|
| Executor Linux host | Executor, credential-free supervisor, operations probe and protected transport. No KMS signing permission. |
| Independent recovery Linux host | Signer, KMS adapter/AWS CLI, watchdog and protected transport. Workload role restricted to `kms:Sign` and `kms:GetPublicKey` on the reviewed key. |
| Independent monitoring and operator | Real delivery acknowledgements, recovery-access checks, host/transport monitoring and manual account recovery. Must remain available after executor loss. |

Separate availability zones are the AWS reference layout. Use private service
identities, encrypted persistent state volumes, synchronized clocks and no public
signer listener. Preserve a single durable nonce/reservation authority for the
agent on the recovery host. A second signer with fresh state is not failover.
AWS, KMS, venue and liquidity dependencies remain shared failure risks.

Build on the intended Linux image/architecture and use [verified release bundles](release-bundles-v1.md)
to stage/install the executor or recovery role. Installation is passive. The supplied
systemd units do not provision IAM, keys, SSH, monitoring or transport. Do not enable
them until their configuration and startup sequence have been reviewed. Pin the
actual AWS CLI executable/configuration used under the service identity; a working
interactive administrator shell does not prove the service can access KMS.

## 3. Provide protected transport and honest monitoring

Provision authenticated transport appropriate to your hosts, such as SSH with
pinned host keys and restricted service credentials. Review forwarding/replication
permissions explicitly; the templates do not create this infrastructure.

| Channel | Required behavior |
|---|---|
| Executor to recovery signer | Owner-only local Unix socket forwarded over authenticated encrypted transport; no unauthenticated TCP exposure. Bound reconnect and request deadlines. |
| Executor lease/health to recovery | Replicate atomically; preserve the original modification time and payload timestamps. Only the executor renews its lease. A transport timer must not make stale data fresh. |
| Watchdog heartbeat to executor | Preserve original modification time, use private paths and atomic replacement; prove stale or partitioned transport fails closed. |
| STOP | Deliver and latch on both hosts. Restart, deployment and replication must never clear an incident's STOP automatically. |
| Monitoring receipts | Independently timestamp real alert delivery and recovery access; retain the final runtime digest and recovery artifact identity. |

Do not replicate signer nonce/reservation files as independently writable state.
Keep journal and emergency-state ownership unambiguous. Backups must preserve
consumed state, permissions and incident evidence. Restoring an older snapshot
must never re-enable a consumed placement.

The shipped [operations probe](operations-probe-v1.md) validates delivery and
recovery receipts plus the retained artifact. Integrate the receipt writer with
your actual monitoring infrastructure; generating successful JSON on a timer
does not establish delivery or recovery access. Use owner-only directories,
atomic writes and preserved observation timestamps. The probe and doctor do not
send alerts, create successful receipts or grant release approval.

## 4. Bind configuration and run offline diagnostics

Copy example JSON into private configuration directories and replace every
placeholder with reviewed values. Choose your account, agent, exact canary,
emergency collar, risk limits, host paths and freshness/time budgets together.
Measure actual response times before finalizing limits. The watchdog's scheduled
cancel re-arm cadence must fit the account's available action budget for the full
session, including recovery actions.

Follow [the binding order and doctor commands](setup-diagnostics-v1.md). Pin
provider/probe executables and configuration; bind service and supervisor policies
before the runtime and gate digests. Run `hypercarry-doctor` against the actual
installed bytes and `hypercarry-kms-provider --check-config` under the service
identity. Passing these commands proves local consistency only. They do not
contact AWS, verify the agent's venue authorization, or establish live readiness.

The supervisor intentionally refuses capped or ambiguous account history. Use a
dedicated account and investigate a refusal instead of raising counters manually.
Its closed-P&L observation is not full account equity or fee/funding reconciliation;
retain independent account monitoring. Never edit a ready health file to bypass
a failed check. See [supervisor behavior](supervisor-v1.md).

## 5. Exercise the deployment before release

Retain secret-free logs, exact artifact/configuration identities, measured delays,
before/after account observations and independent reviewer decisions.

| Exercise | Required outcome |
|---|---|
| KMS under the real service identity | Correct address/signature; wrong identity, denied/revoked access, timeout and malformed output fail closed. No raw key export or double hashing. |
| Restart and concurrency | Durable nonce, placement reservation, journal and emergency state survive; duplicate processes and uncertain retries cannot place a second entry. |
| Executor loss or transport partition | Lease expires, heartbeat withdraws, STOP latches and independent recovery runs. Stale replicated files do not renew authority. |
| Private-stream disconnect, duplicates and latency | Supervisor withdraws readiness, reconciles with REST and only becomes ready when fresh observations agree. Include all perp DEXs. |
| Partial fill and cancel race | Recovery checks both orders and positions, attempts bounded reduce-only IOC, then re-queries. A canceled order alone is insufficient. |
| Recovery/signing/venue failure or exhausted collar | No false flat/success claim; independent alert reaches a person with manual recovery access. Scheduled cancellation cannot close an already filled position. |
| Receipt/artifact failure | Missing, stale, future, unsuccessful or wrong-runtime receipts and changed artifact bytes prevent readiness. |
| MCP if enabled | Data writes cannot reach config, approvals or state; expired, replayed and mismatched consent cannot execute. |

Produce at least three distinct credentialed, independently reviewed testnet
sessions using [the operator harness](testnet-operator-v1.md) and
[session review runbook](testnet-session-review-runbook.md). Cover the documented
faults; three repeated clean runs do not replace failure evidence. Obtain fresh
independent review of [the signing migration](signing-migration-review.md), custody,
supervisor, watchdog, transport and consent boundaries at the intended release.

Before invoking the executor's manual `recover`, explicitly coordinate with the
watchdog: recovery mode does not renew the normal executor lease. Stop the watchdog
first or review the intended concurrent behavior. Preserve STOP and consumed state;
never delete them to retry an uncertain order. See [operator recovery](operator-recovery.md)
and [service recovery](mainnet-services-v1.md).

## 6. Freeze and approve one exact canary

After the exercises and review pass, freeze the final public source commit,
lockfile, native Linux binaries, configuration, provider identity, evidence and
rollback artifact. Re-run CI and dependency checks against that frozen graph.
The filtered public commit differs from the private development commit; build
and approve the public identity, not binaries from its private predecessor.

Use the offline digest commands to prepare schema-v2 evidence and obtain the
named, timestamped human decision required by [the release gate](mainnet-release-gate-v1.md).
Digest commands calculate bindings; they cannot supply the human review. Run
static preflight, then verify live signer, watchdog and supervisor readiness.
Follow the startup/approval sequence in [the launch runbook](mainnet-launch-runbook.md)
for the exact minimum-size canary. Retain final all-DEX order/position reconciliation.
Any changed account, order, artifact or bound policy needs renewed review/approval.

Before publishing source, run `python3 -B tools/check_public_export.py` and an
independent secret/content review. The checker covers known credential formats,
local artifact paths and inline documentation links; it is not a complete secret
scanner. The filtered export workflow never transfers private Git history. Final
publication and live operations are separate release actions.
