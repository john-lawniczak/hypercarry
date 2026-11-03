# Release review dossier

`tools/release_readiness.py` freezes a candidate and checks the integrity of
operator-supplied evidence. It never signs, contacts a venue, creates a release
approval, writes operational receipts or declares a human review completed.
Every output has `trading_authorized: false`. A complete dossier is material for
a human release decision, not a substitute for the existing executor gates.

## Freeze a candidate

First complete [static configuration binding](setup-diagnostics-v1.md) and the
[cloud preparation](cloud-validation-v1.md). Build a clean-source Linux GNU bundle
with [release_bundle.py](release-bundles-v1.md), using the final intended public
source identity after separately authorized publication/export review. Development
rehearsals can use private-source bundles, but their hashes and evidence do not
approve a different public commit. This tool verifies a bundle's recorded commit;
it cannot establish whether that commit has been publicly published or reviewed.

Keep your six JSON files in a trusted, private directory. `runtime` is the
standalone runtime object used by the existing digest commands; `executor` is
the enclosing executor configuration. Use the doctor to validate their semantic
bindings. The dossier stores exact bytes, so even formatting changes require a
new candidate. It does not replace Rust configuration validation.

```sh
python3 tools/release_readiness.py prepare \
  --bundle /private/work/release.tar.gz --sha256 "$BUNDLE_SHA256" \
  --config executor=/private/work/config.json \
  --config runtime=/private/work/runtime.json \
  --config services=/private/work/services.json \
  --config supervisor=/private/work/supervisor.json \
  --config kms=/private/work/kms.json \
  --config operations=/private/work/operations.json \
  --output /private/work/candidate
```

The absent output directory is created private (0700), with private files (0600).
It contains a verified snapshot of the release bundle, six configuration snapshots,
`candidate.json` and an explicitly pending inventory. Record the printed
`candidate_sha256` through your trusted review channel. No existing candidate is
overwritten. Inputs must be regular files; JSON rejects duplicate keys/non-finite
numbers and has a 4 MiB per-file limit. Only Linux GNU bundles with both roles'
required programs and units can be frozen. The AWS reference uses x86_64.

Configuration may contain sensitive account and infrastructure identifiers even
without credentials. Keep the whole dossier and evidence outside Git; it is not
part of the public source export. Protect the parent directories from untrusted
writers. Hash integrity is not authentication if an attacker can also replace
the expected hash supplied by the reviewer.

## Retain actual evidence

Use the [observation record example](release-observation.example.json) as a
schema reference. Its `pending` outcome is deliberately rejected by verification.
After a witnessed exercise or independent review, supply a separate record with:

- Exact `candidate_sha256` and one of the twelve required scenario names.
- Actual `passed` or `failed` outcome, UTC `performed_at` ending in `Z`, operator,
  distinct reviewer, and concrete observations (including failures and limitations).
- At least one retained artifact, referenced relative to the record directory with
  its exact SHA-256. No absolute paths, traversal, symlinks or duplicate paths.
  Retain originals separately when logs exceed 4 MiB; attach reviewed excerpts
  and a digest/index of the originals, not claims that truncated logs are complete.
- A distinct `session_id` for each of the three testnet sessions, otherwise null.

The required scenarios are `security_review`, `kms_access`, `kms_signing_faults`,
`transport_partition`, `executor_loss`, `private_stream_disconnect`,
`partial_fill_recovery`, `alert_delivery`, `rollback_drill`, `testnet_session_1`,
`testnet_session_2` and `testnet_session_3`. The [cloud exercise matrix](cloud-validation-v1.md)
defines operational acceptance observations. Attach the existing testnet operator
review attestations and their original artifact manifests for all three sessions.
The dossier validates record integrity, not the internal semantics of every
external report; the release reviewer must examine them and run existing gates.

For `security_review`, retain an independent report covering secp256k1/k256 and
EIP-712/MessagePack/domain handling, KMS DER/low-S recovery and exact digest use,
provider timeout/process isolation, authenticated socket transport, IAM/IMDS trust,
nonce and journal durability, all-DEX reconciliation, uncertain/partial fills,
watchdog STOP/lease behavior and manual recovery. Include findings and dispositions,
reviewer identity and exact source/binary/configuration scope. Do not represent an
automated local audit as this independent review.

For `rollback_drill`, retain the prior known-good bundle/config hashes, state and
nonce/consumed-approval preservation checks, independent flatness reconciliation,
STOP status, installation logs and post-restore doctor results. A fresh approval
must bind any changed binaries/configuration before entry resumes. Software
rollback must never roll nonce/journal/consent state backward.

## Verify before review

Use the trusted **candidate manifest** digest here, not the archive checksum.
Supply the current installed/configuration copies to detect drift. Repeat
`--record SCENARIO=PATH` for each supplied observation:

```sh
python3 tools/release_readiness.py verify \
  --candidate /private/work/candidate --sha256 "$CANDIDATE_SHA256" \
  --config executor=/private/work/config.json \
  --config runtime=/private/work/runtime.json \
  --config services=/private/work/services.json \
  --config supervisor=/private/work/supervisor.json \
  --config kms=/private/work/kms.json \
  --config operations=/private/work/operations.json \
  --record security_review=/private/evidence/security/record.json \
  --record rollback_drill=/private/evidence/rollback/record.json \
  --report /private/work/review-report.json
```

This partial example exits **2**, lists missing scenarios and writes the report
exclusively. A complete set of passing records exits **0** with
`evidence_complete_for_review: true`; invalid/mutated inputs exit nonzero. Failed
exercises stay visible. The checker refuses reused session IDs, identical
operator/reviewer names and future dates, but cannot prove independence, delivery,
review quality, session authenticity or ongoing readiness. Evidence has no automatic
freshness guarantee: reviewers must decide which observations remain applicable.
Real-time health/receipt expiry is enforced separately by runtime code.

Retain the report, exact record files and referenced artifacts together; the
report binds each record hash but does not copy external evidence. A configuration,
source or binary change requires a new candidate and explicit reassessment of
which evidence must be rerun. Finally use the existing
[release gate](mainnet-release-gate-v1.md), digest/preflight and separate human
approval workflow. A dossier report is intentionally not a runtime approval file.
