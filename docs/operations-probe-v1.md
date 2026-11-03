# Portable operations probe v1

`hypercarry-operations-probe` is a credential-free, read-only implementation of
the supervisor probe protocol. It checks actual monitoring receipts and retained
recovery bytes. It does not send notifications, test account access itself,
create receipts, write health, or authorize trading.

Install it alongside the supervisor. Start from
[operations-probe-config.example.json](operations-probe-config.example.json),
replace the recovery artifact hash with the reviewed watchdog binary hash, and
choose receipt age bounds supported by measured monitoring and replication
latency. Keep the receipt directory private (0700). The receipt writer and
protected transport are trusted parts of the deployment and require review.

Configure `operational_probe` in the supervisor JSON with the executable,
`sha256`, `timeout_ms`, `config` and `config_sha256`. Both hashes cover exact file
bytes. The supervisor verifies them before each invocation and passes only:

```text
hypercarry-operations-probe --config /etc/hypercarry-canary/operations.json --challenge CHALLENGE --integration-config-digest RUNTIME_DIGEST
```

The optional config fields are omitted from historical supervisor digests when
absent. Adding them requires rederiving the supervisor/runtime/gate/evidence
bindings. The probe config does not contain a runtime digest: that would create
a circular hash dependency. The **receipts** must name the final runtime digest.

## Monitoring receipt contract

After an authenticated, confirmed end-to-end delivery check, the monitoring
adapter writes `alert.json` atomically. Do not emit a receipt merely because a
webhook accepted a request. Retain the actual provider/recipient delivery ID:

```json
{
  "schema_version": 1,
  "integration_config_digest": "FINAL_RUNTIME_SHA256",
  "delivered_at_ms": 0,
  "delivered": true,
  "delivery_id": "ACTUAL_CONFIRMED_DELIVERY_ID"
}
```

An independently exercised recovery-access check writes `recovery.json`:

```json
{
  "schema_version": 1,
  "integration_config_digest": "FINAL_RUNTIME_SHA256",
  "checked_at_ms": 0,
  "access_verified": true,
  "check_id": "ACTUAL_RECOVERY_ACCESS_CHECK_ID",
  "artifact_sha256": "REVIEWED_WATCHDOG_SHA256"
}
```

These examples deliberately fail freshness/binding checks. Replace timestamps
only after the corresponding real checks succeed. Never refresh old receipt
contents with a timer or a copy operation. The probe uses embedded observation
times, not file modification times, so recopying an old receipt cannot renew it.
Do not page a person every supervisor poll; check current delivery receipts and
exercise end-to-end delivery at the reviewed monitoring cadence.

The probe requires both receipts to be successful, nonempty, nonfuture, within
their independent age limits (configured from 1 second to 1 hour), and bound to
the requested runtime. The recovery receipt must name the configured artifact.
It then hashes the actual regular, non-group/world-writable recovery file. A
missing/changed artifact, missing receipt, unknown schema/field, wrong runtime,
stale/future timestamp, failed delivery/access check or invalid hash exits
nonzero and produces no ready response. Receipt/schema input is bounded to 1 MiB.

Receipt freshness is evaluated after reading receipts and hashing the recovery
artifact, so slow artifact I/O cannot preserve an earlier freshness decision.

Successful output uses the existing challenge-bound supervisor protocol. That
proves the current invocation checked the receipts; it does not upgrade those
trusted receipts into independent evidence of their own truth.

## Cloud exercise before release

1. Integrate the actual monitoring service and recovery operator/access check
   with the receipt writer. Keep secrets in the monitoring infrastructure, not
   in receipt JSON or this repository.
2. Verify real delivery and recovery access on the intended hosts; preserve
   the original timestamps when replicating receipts.
3. Demonstrate rejection for each stale, future, failed, mismatched and missing
   receipt, and for altered recovery bytes. Restore the reviewed bytes/receipts
   through the normal process; do not edit health to bypass the result.
4. Revoke the monitoring/recovery access or interrupt its transport and confirm
   readiness expires within the configured bounds and the executor refuses it.
5. Retain these observations with the independent security and release review.
