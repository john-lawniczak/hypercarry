# Local bracket mainnet pilot — gated implementation

Status: **implemented, default off; no live release approved**. The separate
`tools.local_operator.mainnet` adapter implements this attended profile. It does
not alter or satisfy the existing single-GTC canary gate. Qualification of the exact
candidate and independent approval of its fresh mainnet plan remain required.

## Enforced narrow scope

- Attended session with a dedicated approved mainnet agent, distinct from testnet
  and the owner wallet. Owner approval of that agent occurs in the official UI;
  the executor never loads the owner key.
- One native USDC perpetual market, isolated 1x, exact approved side and quantity,
  at most $20 planned budget and quantity × entry-limit bound, and explicit
  free-USDC reserve from a fresh plan. A favorable short fill at a higher price
  can have higher actual notional. No trade is approved by this profile alone.
- Exact plan binds source/lock/SDK/binary hashes, account, agent, market asset ID,
  size, entry collar, TP/SL triggers and execution bounds, reserve, expiry, and
  permitted actions. Any change invalidates approval.
- Native full-size reduce-only TP/SL must be observed after entry. Close is a
  separately approved reduce-only IOC. Cancel only explicitly owned exits after
  authoritative flatness. Preserve unresolved IDs and account reservations.
- Entry requires a flat account with no open orders across all discovered DEXs.
  Mainnet close accepts only a preserved position record from this pilot's own
  entry, matching its durable plan. Existing positions cannot be adopted into it.
- The operator attends throughout, with a separate way to view the venue and
  perform manual recovery if the local host fails. A console alert is local
  display evidence only. It does not demonstrate notification delivery or recovery
  independent of the local machine. Unattended operation needs that additional
  provisioning and evidence.

## Evidence required before the release decision

1. Freeze a clean source commit and exact dependency/SDK/binary identities;
   retain passing offline checks and the current dependency audit.
2. Retain three distinct credentialed sessions through the new testnet path:
   full lifecycle, partial lifecycle, and fault/restart reconciliation. Include
   original requests without signatures, journal and nonce reservations, actual
   venue statuses/fills, independently obtained final positions/open orders,
   residual accounting, and human review. Scripted faults must be labeled as
   scripted, not presented as real venue failures.
3. Exercise recovery using replacement IDs and a builder market when available;
   explicitly document unavailable testnet markets/liquidity. Validate stop races,
   lost acknowledgements, restart, stale scope, agent expiry/revocation and signed
   action budgets. No duplicate entries, unmanaged orders, unresolved submissions
   or secret leaks may be waived away by an offline test count.
4. Confirm an operator-visible terminal alert, failure detection and manual
   fallback for this attended operating model. Independent liveness/recovery is
   required before claiming unattended availability.
5. Obtain independent signing/custody and implementation review, including Python
   key lifetime, SDK MessagePack ordering, fresh stream bootstrap/REST boundaries,
   account locks/freezes, unknown submissions, nonce budgets, and recovery policy
   derivation from immutable journals.
6. Reconcile a read-only mainnet window and preserve unexplained differences.
   Scoped Unified-USDC accounting is not full lifetime or spot-hedge accounting.
7. Review the default-off adapter for this exact profile; approve a release
   artifact binding all of the above. Do not reuse the single-GTC capability or
   flip a network string to enable bracket submission.

## Candidate and release contract

First use `freeze-candidate` as described in the
[testnet runbook](attended-testnet-session.md#freeze-the-qualifying-candidate).
It builds release binaries from clean committed source and records the source
commit, Cargo.lock hash, Python version/package inventory, SDK 0.24.0 and
analytics/journal/recovery binary paths and
hashes. Bind all three sessions to that same candidate; source or artifact changes
require a new qualification decision. No freeze command grants trading authority.

After qualification, `preview-open --candidate FILE --candidate-sha256 HASH
--input INPUT --agent ADDRESS --key-id ALIAS --output PLAN` creates an unsigned
mainnet plan and sibling `.policy.json`. Use the integration guide's entry input
schema with `network: mainnet`. A plan expires after five minutes. A subsequent
`preview-close` uses `--adoption OPERATIONS/ENTRY_SESSION/position.json` instead
of `--input`; it prepares a full residual close and preserves entry ownership.
Both run through `python -m tools.local_operator.mainnet` with the pinned SDK.

The private release JSON is reviewed externally, then its canonical SHA-256 is
supplied explicitly. It must contain exactly these fields:

| Fields | Required value |
| --- | --- |
| `schema_version`, `kind`, `decision` | `1`, `attended_mainnet_bracket_release`, `approve` |
| `candidate`, `plan_digest` | Frozen manifest and canonical digest of the complete sealed plan |
| `account`, `agent`, `testnet_agent` | Exact plan identities; dedicated mainnet agent distinct from owner/testnet agent |
| `operations_dir`, `signer_dir`, `stop_file` | Fixed canonical absolute paths without symlinks; separate private state directories |
| `expires_at_ms` | Future Unix milliseconds, within one hour, covering policy expiry |
| `operator`, `reviewer` | Named human approvers, distinct case-insensitively |
| `checks` | `offline_tests`, `dependency_policy`, `advisory_audit`, all true with retained supporting receipts |
| `evidence` | Six records, exactly one per scenario below |

Each evidence record contains exactly `schema_version: 1`, `candidate_sha256`,
`scenario`, `outcome: passed`, `operator`, `reviewer`, `performed_at_ms`,
`observations`, `artifacts` and `session_id`. Artifact entries are relative
`path`/`sha256` pairs under the release directory; paths cannot escape or traverse
symlinks. Scenarios are `full_lifecycle`, `partial_lifecycle`, `fault_restart`,
`security_review`, `alert_and_manual_recovery`, `mainnet_accounting`. The first
three need distinct nonempty session IDs; the others use JSON null. Reviewers
must differ from operators. Retain actual unsigned journals, venue observations,
accounting, test receipts and review notes; do not substitute example records.
Canonical hashing uses `tools.local_operator.store.digest`, not raw JSON bytes.

`check-release`, `signer` and `execute` each require `--enable-mainnet --release
FILE --release-sha256 HASH --plan PLAN`. The signer additionally requires
`--keystore FILE`, displays the policy digest, and prompts locally for approval
and decryption. Execution derives its socket and state directories from the
release. Close needs a separate exact-plan release. The software checks evidence
bindings and supplied reviewer identities; it cannot establish whether human
claims are truthful or whether two names represent independent people.

The adapter rechecks candidate/evidence/STOP and the maintenance-exception deadline
at signing and final submission,
verifies the mainnet signing domain and current agent approval, repeats live
entry risk/capacity checks, and durably consumes each wire attempt before HTTP.
It never retries a lost acknowledgement. Creating `stop_file` prevents further
local signing/submission, including close/repair; it does not cancel venue-native
exits. Preserve state and use unsigned `tools.local_operator reconcile` after
uncertainty, even if a release expired. Verified reconciliation preserves owned
position evidence for a later close. Use the independently available manual venue
path if local recovery cannot proceed. Automated Rust recovery remains testnet-only;
the local pilot does not provide unattended availability or exit amendment.

## Pilot acceptance and rollback

The actual pilot requires a fresh reviewed plan and explicit mainnet approval.
Success means independently observed entry fill, matching native exits, bounded
reduce-only close, final flat target and cleaned owned exits, no unresolved
submissions, and archived fees/funding/accounting. Acknowledgement alone is not a
fill. Stop on uncertain state, drift, agent failure, missing protection or reserve
breach; preserve native exits and durable reservations while reconciling. Never
repeat an uncertain submission or delete its state to retry. Do not inject live
mainnet failures or cancel existing stops to prove recovery.

After this first native-market pilot, builder-market support needs its own scoped
review. Carry-product approval additionally requires spot-hedge execution and
both-leg accounting; a directional short does not complete those requirements.
