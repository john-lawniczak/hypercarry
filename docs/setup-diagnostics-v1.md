# Offline setup diagnostics

Use the diagnostics during infrastructure preparation, before you have release
approval or live receipts. They never contact the venue, execute a signing
provider, write journals/health, reserve nonces or create approvals.

```sh
hypercarry-doctor \
  --config /etc/hypercarry-canary/config.json \
  --policy /etc/hypercarry-canary/services.json \
  --supervisor /etc/hypercarry-canary/supervisor.json \
  --artifacts-dir /opt/hypercarry/bin

hypercarry-kms-provider \
  --config /etc/hypercarry-canary/kms.json --check-config
```

Run after creating private directories and installing the actual binaries/config
files. Use absolute paths. The doctor prints a schema-v1 JSON report and exits
zero only when every static check passes; otherwise it exits nonzero with failed
check names/details. `trading_authorized` is always false. An unreadable or invalid
base configuration returns a report without attempting dependent checks.

Checks cover runtime/gate validation, disjoint authority/state paths, shared
executor identity, exact service/supervisor digests, executable hashes and
permissions, optional provider/probe configuration hashes, configured operations
probe, nested backend/signer/action timeouts, and sampling/probe bounds that leave
room inside configured freshness limits. They do not measure actual latency.
Artifact checks use the signer/watchdog bytes in `--artifacts-dir`, so run them
against each intended installed release, not an unrelated build directory.

The KMS static check validates the immutable key ARN/region relationship,
provider identity and pinned executable. It reads no signing request and never
invokes AWS. It supports AWS, GovCloud and China ARN partitions, but does not
prove account access, IAM policy, key state, workload credentials, network access
or agent authorization. Those require the later cloud exercise.

## Prepare bindings without approval

Use this order to avoid circular digests:

1. Install the binaries and record their SHA-256 hashes. Prepare KMS/operations
   configuration with actual paths, identities, recovery hash and age limits.
2. Put backend/config hashes into `services.json`; put probe/config hashes into
   `supervisor.json`. The supervisor names the executor config **path**, not its
   content hash. Operations config names receipt paths; receipts later name the
   final runtime digest.
3. Compute service and supervisor digests:

   ```sh
   hypercarry-mainnet-signer --policy /etc/hypercarry-canary/services.json --policy-digest
   hypercarry-supervisor policy-digest --config /etc/hypercarry-canary/supervisor.json
   ```

4. Set `runtime.services_policy_digest`, `runtime.supervisor_policy_digest` and
   `runtime.executor_lease`. Compute the runtime digest from its standalone JSON
   with `hypercarry-executor config-digest --runtime PATH` and put it into the
   enclosing gate's `integration_config_digest`.
5. Run the doctor and KMS static check. Correct configuration or installed bytes
   and repeat binding preparation if any pinned input changes. Do not loosen risk
   or latency limits just to get a passing report.
6. After independent review and operational testing, use `gate-digest` and
   `evidence-digest` to prepare the exact release decision. Those commands compute
   hashes; they do not grant approval. The existing `preflight` requires complete
   independently reviewed evidence and clean-build identity.

A clean static report cannot prove delivery, recovery access, private-stream
liveness, account flatness, clock synchronization, rate-limit capacity, clean
Linux build identity or a human release decision. Keep these separate and retain
both static reports and measured cloud evidence with the reviewed bundle.

## Read-only cloud identity check

After provisioning, `hypercarry-kms-provider --config PATH --check-access`
executes the pinned AWS CLI for GetPublicKey under EC2 workload identity. It
checks the returned immutable ARN, signing algorithm, validated secp256k1 point
and configured address. The process group, output size and elapsed time are
bounded. It neither reads a signing request nor invokes Sign; the report leaves
`sign_permission` and `venue_agent_authorization` as `not_checked`. Unlike
`--check-config`, this command contacts AWS. See [cloud validation](cloud-validation-v1.md).
