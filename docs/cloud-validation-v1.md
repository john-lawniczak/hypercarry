# AWS reference preparation and validation

The [CloudFormation template](../deploy/mainnet/aws-reference.json) creates a
**passive** pair of private Linux hosts and a retained secp256k1 KMS signing key.
It does not install software, mount/format state disks, establish transport,
produce monitoring receipts, approve execution or start trading. Review a change
set before provisioning. Provisioning and retained resources incur AWS charges.

## Prepare the change set

Provide an AWS account/region, an existing VPC, two private subnets in different
availability zones, and a reviewed x86_64 Linux AMI. The reference instance types
are `m6i.large` and `m6i.xlarge`; this bounds the architecture choice. The AMI must
have a `/dev/sda1` EBS root, systemd, a working SSM agent, and an approved patching
baseline. Verify its publisher account independently. These are operator inputs,
not recommendations for a particular uninspected image.

Copy [the settings example](aws-reference-settings.example.json) to a private
working directory and replace every placeholder. With your read-only AWS session,
export inventory for the selected IDs (set the variables to your own values):

```sh
umask 077
aws ec2 describe-subnets --region "$AWS_REGION" \
  --subnet-ids "$EXECUTOR_SUBNET" "$RECOVERY_SUBNET" > subnets.json
aws ec2 describe-images --region "$AWS_REGION" \
  --image-ids "$IMAGE_ID" > images.json
python3 tools/prepare_aws.py --settings /private/work/settings.json \
  --subnets /private/work/subnets.json --images /private/work/images.json \
  --output /private/work/aws-plan
```

Use a real private directory on your OS; `/private/work` is illustrative. The
command snapshots all inputs, validates VPC/AZ separation and AMI compatibility,
and writes hashed template/parameter files to an absent directory. It makes no
network calls. Inventory authenticity and freshness remain your responsibility;
regenerate inventory immediately before preparing a change set.

From that frozen plan, prepare but **do not automatically execute** a change set:

```sh
aws cloudformation create-change-set --region "$AWS_REGION" \
  --stack-name hypercarry-canary --change-set-name reviewed-candidate \
  --change-set-type CREATE --capabilities CAPABILITY_IAM \
  --template-body file:///private/work/aws-plan/template.json \
  --parameters file:///private/work/aws-plan/parameters.json
```

Review effective IAM, costs, the AMI owner/content, routes and the change set.
Use the operator's normal approval process before executing it. The template
has no public IPs or public ingress. Peer port 22 permits later authenticated
transport; it supplies no keys or host-trust policy. HTTPS egress is allowed for
AWS/venue APIs. The VPC must supply DNS, clock synchronization, NAT or appropriate
endpoints, and SSM connectivity. Separate subnet IDs alone do not prove separate
AZs; the preparation tool checks their stable AZ IDs.

Both hosts require [IMDSv2](https://docs.aws.amazon.com/AWSCloudFormation/latest/TemplateReference/aws-properties-ec2-launchtemplate-metadataoptions.html).
Recovery gets `kms:GetPublicKey`/`kms:Sign` for the created key; executor has an
explicit `kms:Sign` deny. Account administrators retain KMS administration and
IAM delegation. Root, SSM administrators and privileged processes on recovery
are custody-trusted: IMDSv2 does not isolate mutually untrusted local processes.
The template's SSM management permission does not grant access to your human
identities; restrict their session and IAM permissions separately.

The KMS key and both encrypted state volumes use `Retain` on deletion and
replacement. Retention is not a backup. Preserve snapshots and state identity;
never initialize a replacement signer with an empty nonce/intent history.
Map actual EBS volume IDs to NVMe devices, inspect for existing filesystems,
mount at `/var/lib/hypercarry-canary`, and add a systemd mount dependency before
starting services. Do not format a disk as part of a retry. A service must not
silently fall back to the root filesystem if the state mount is absent.
Install [the mount assertion drop-in](../deploy/mainnet/aws-state-mount.conf) as
`/etc/systemd/system/UNIT.service.d/10-state-mount.conf` for each installed
Hypercarry unit, then reload systemd. It requires the mount and makes absence a
start failure. Verify the mounted volume ID and permissions independently.

## Install and measure before funding

1. Use the [verified bundle installer](release-bundles-v1.md) on each role.
   Pin the AWS CLI and all executable/configuration hashes. Complete the
   [binding/doctor sequence](setup-diagnostics-v1.md). Keep all services passive.
2. Obtain the new KMS public key, derive its agent address, and configure the
   provider with the immutable ARN and that address. On recovery, run as the
   actual service identity:

   ```sh
   hypercarry-kms-provider --config /etc/hypercarry-canary/kms.json --check-access
   ```

   Retain stdout and exit status. This bounded diagnostic uses only
   [GetPublicKey](https://docs.aws.amazon.com/kms/latest/APIReference/API_GetPublicKey.html),
   verifies the ARN, curve, usage, algorithm and address, and never calls Sign.
   A successful report proves neither signing permission nor venue agent approval.
   Credential/config-file environment overrides are cleared; it tests EC2 workload
   identity. Follow the existing testnet provider workflow for actual signing
   exercises; the mainnet KMS adapter does not accept testnet signing requests.
3. Establish authenticated, host-key-pinned transport with reviewed account/port
   restrictions. Preserve payload timestamps and file mtimes during replication,
   replace files atomically, and propagate missing/expired health as refusal.
   Do not expose a signer socket over unauthenticated TCP. Follow the
   [service contract](mainnet-services-v1.md) for socket, lease and STOP semantics.
4. Integrate an independent monitoring adapter that verifies actual delivery and
   recovery access, then writes the [receipt contract](operations-probe-v1.md).
   A webhook HTTP 200 or a running watchdog PID is insufficient. Record real
   delivery/check IDs; this repository intentionally cannot manufacture receipts.
5. Execute the following witnessed exercises in an isolated testnet/rehearsal
   environment first. Retain UTC timings, hashes, journal/health transitions,
   bounded response durations, final account state and the observer's review.
   Use the [release dossier](release-review-dossier-v1.md) to bind the results.

| Dossier scenario | Exercise and required observation |
|---|---|
| `kms_access` | Run read-only identity check from recovery; retain output, role/key identifiers and effective-policy review. |
| `kms_signing_faults` | Review Sign IAM separately; test denied/slow/malformed signing responses with stubs/testnet custody. Observe bounded refusal, no retry of an uncertain action and descendant cleanup. Retain CloudTrail evidence for any real authorized KMS signing check. |
| `transport_partition` | Interrupt forwarding/replication. Stale health must refuse; the independent recovery path must remain reachable. Resume transport without replaying a consumed approval. |
| `executor_loss` | Stop/kill the executor and simulate host loss. Observe lease expiry, STOP latch and watchdog decision; independently reconcile fills/orders. Restore preserved state, never reset it to get past refusal. |
| `private_stream_disconnect` | Interrupt the private stream, with REST still available. Ready health must be withdrawn; reconnect must resubscribe and reconcile before readiness returns. Measure actual age/batch latency and request rate. |
| `partial_fill_recovery` | With a separately authorized testnet action, retain partial-fill/cancel/reduce-only results and dust/residual position checks. Exercise independent manual recovery when automated flattening cannot succeed. |
| `alert_delivery` | Verify end-recipient delivery and recovery access; stop the receipt writer and replay old/wrong-runtime receipts. They must expire/refuse. Confirm monitoring survives executor loss. |
| `rollback_drill` | Latch STOP, stop entry, reconcile flatness, preserve journals/nonces/consumed approvals and snapshots. Restore the reviewed prior binary/config set without enabling entry. Retain old/new artifact hashes and doctor output; re-review bindings before resuming. |

Three distinct credentialed testnet sessions need the existing offline
[independent review workflow](testnet-operator-v1.md). Rehearsal results do not
replace final deployment validation. Any mainnet fault injection or trade needs
its separate reviewed plan and existing gate approval. Do not run destructive
failure exercises against a funded account merely to fill a checklist.

## Handoff inputs

For provisioning review, supply public account/region/VPC/subnet/AMI identifiers,
AMI publisher, the two inventory exports, intended monitoring/delivery service,
transport design and independent reviewer identity. Use workload roles or your
normal temporary access mechanism; do not put keys, credentials, private state
or operational evidence in Git. After deployment, retain the change-set result,
instance/volume/key IDs, doctor and access reports, measured failure evidence and
review attestations outside the checkout. The public software remains scoped to
one reviewed canary with independent recovery.
