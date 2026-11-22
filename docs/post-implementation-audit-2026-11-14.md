# Post-implementation audit — November 14, 2026

Scope: all-DEX observations, archive/publication boundaries and the fixes
accompanying this report. Reviewed changed Rust/Python/shell code, callers, nearby account wire types,
filesystem/publication helpers, tests and the changed operator documentation.
Private records were reviewed in the private repository; no trading records or
account identifiers are reproduced here. This is an implementation review, not
independent release approval or a review of the ignored experimental trader.

## Findings and applied fixes

| ID / severity | Concrete defect and likely impact | Applied fix / regression evidence |
| --- | --- | --- |
| A01 / High | The original public exporter only removed root-level `Z-*.md`. Lowercase, nested and non-Markdown private files could survive. Gitignore alone cannot protect already tracked files/history. | Shared recursive Z-/z- and private-directory policy; matching ignore/archive rules, actual-tree checks and a private-history push guard. Real Git tests force-track nested/case-varied fixtures, then verify ignore and archive behavior. Existing private journals stay in the private repository. |
| A02 / High | Freshness checks ran only when a DEX had an open position. A stale empty response could produce a successful flat/protected check. | Validate venue timestamps on both reads of every DEX, including flat states. Tests cover stale emptiness and future timestamps. |
| A03 / High | A single position read followed by an order read could compare exits with an old quantity or direction after a resize/close. | Read positions before and after orders; reject changed quantity, entry or margin identity. Preserve both reads in schema 2. Regression cases grow/flip/close positions and change margin mode; normal mark/P&L movement remains allowed. This narrows races without claiming atomicity. |
| A04 / Medium | The elapsed-time check used wall time and ran only after requests returned. A slow in-flight read could exceed the advertised run budget, and clock changes could undermine elapsed-time reasoning. | Wrap the whole observation in a monotonic Tokio timeout, retaining wall-clock reversal/source-time checks. A permanently pending transport regression proves cancellation at the configured deadline. |
| A05 / Medium | Builder prefixes were validated only for builder responses. A misrouted builder position/order in the native response was accepted. | Explicit native/builder scope validation, duplicate identity rejection and control-character checks. Tests inject builder records into native responses. |
| A06 / High | The first push guard parsed `git rev-list --objects` by line. Git truncates object paths containing newlines, so a private file under such a directory could evade the guard, including after deletion. | Enumerate distinct reachable trees and inspect `git ls-tree -z` paths. A real temporary Git history with a newline directory and deleted private file reproduces and closes the bypass. Destination mismatch/missing config still fails closed. |
| A07 / Medium | `O_NOFOLLOW` on a directory path ending in `/.` or `/` did not necessarily protect the symlink component the operator intended as the final directory. | Normalize trailing path components before opening; retain the pinned descriptor for file creation/publication. Regressions cover direct, slash and dot symlink aliases and nonprivate permissions. |
| A08 / Medium | The new archive serializer had no size budget and did not bind snapshot schema/timestamps to the outer observation. A caller error could preserve misleading chronology, and oversized responses could consume unbounded archive-buffer memory. A simultaneous read/storage failure hid the read cause. | Bound serialization to 16 MiB before creating a file; bind account/network/schema/time and coverage status; retain the read error when storage fails. Tests cover oversized input, wrong account/time, unknown coverage on failure, private permissions and concurrent immutable publication. |
| A09 / Medium | The account order projection discarded client IDs, venue order timestamps, trigger-condition text and pending children. Archiving that projection permanently lost useful evidence for correlating local intents and investigating missing exits. | Retain these optional fields without treating pending children as active independent exits. Round-trip and coverage regressions verify both evidence retention and conservative treatment. Archives remain normalized projections, not complete raw protocol transcripts. |
| A10 / Low | Funding notes still used present-tense statements from before the completed entries and said the wallet identity had not been verified. Operators could mistake those historical facts for current state. | Label the old snapshot explicitly, point to the subsequent journal review, and distinguish the successful user-local identity check from an unverified current file-mode check. Historical amounts and evidence remain intact. |
| A11 / Medium | The common event envelope labeled every fills/funding response native-only, even though the fills API returns builder and spot fills too. Downstream tracking could silently discard or misclassify those records. | Use request-specific scope labels and retain the explicit recent-fill history limit. A regression checks the envelope scope choices. The official [info API](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint) documents native, HIP-3 and spot fill examples. |
| A12 / Low | Existing success/identity provider tests depended on one-/two-second cold process-launch windows. Full-suite runs intermittently failed without exposing the actual error; isolated runs passed. This made regression failures ambiguous and CI timing-sensitive. | Increase only the success-fixture launch budgets to five seconds and show underlying errors for public fixtures. Dedicated 100 ms timeout/descendant-cleanup regressions and production policy remain unchanged. |

No signer permission, order policy, reserve, production timeout or release gate
was relaxed. The final verification below includes the dedicated timeout and
cleanup regressions as well as the adjusted success fixtures.

Implementation locations:

- [Portfolio reads](../crates/hypercarry-core/src/portfolio.rs): `with_deadline`,
  `read_dex`, `stable_positions` and `scoped_coin` implement A02–A05.
- [Account wire types](../crates/hypercarry-core/src/account.rs): `OpenOrder`
  retains the evidence in A09. [CLI account output](../crates/hypercarry-cli/src/account.rs)
  implements A11 and preserves the read cause on archive failure.
- [Archive storage](../crates/hypercarry-storage/src/observations.rs): `save`,
  `LimitedBuffer` and `publish` enforce A07–A08; CLI failure cases are exercised
  in [integration tests](../crates/hypercarry-cli/tests/portfolio_archive.rs).
- [Export policy](../tools/check_public_export.py), [.gitignore](../.gitignore),
  [.gitattributes](../.gitattributes) and the [push guard](../tools/check_private_push.py)
  implement A01/A06, with real-Git regressions under `tools/tests/`.
- Only fixture budgets/assertions changed in the
  [provider tests](../crates/hypercarry-mainnet-services/src/provider.rs) and
  [KMS tests](../crates/hypercarry-mainnet-services/src/bin/kms_provider.rs) for A12.


## Verification

- **407 Rust tests passed**, with four opt-in live tests ignored; **31 Python
  tooling tests passed**. This includes the real CLI failure/storage-error
  integration tests, paired-read/stale-flat regressions and actual Git fixtures.
- Strict workspace Clippy, formatting, Rust 1.94.1 all-target/all-feature checks,
  all-feature release build, supply-chain deny-list and offline dependency audit
  passed. Only edges to already locked dependencies changed; no version upgrades.
- Public-export content/link checks passed. The normal account CLI dependency
  graph remains free of signer/execution crates.
- An unsigned mainnet schema-2 archive smoke passed with owner-only directory,
  mode-0600 file, retained coverage and SHA-256 receipt. Account details and raw
  evidence remain private. The forced-read-failure smoke produced exit 14,
  unknown coverage, a private failure record and no successful stdout report.

Initial existing provider/KMS success fixtures failed under concurrent cold
launch and passed individually. Test-only success budgets and error diagnostics
were corrected; the final full suite, including separate bounded timeout and
process-cleanup regressions, passed. No production timeout was relaxed.
Mainnet trade execution and real external notifications were not exercised.

The fetched public refs were inspected read-only for private/forbidden paths in
reachable history; eight remote refs had no findings. The owner identifier had
no match in public main's current files or matching historical changes. This is
a scoped check, not proof that no private prose was ever copied under another
filename. No public repository mutation or publication was performed.

## Ship assessment and remaining risks

**Ship rating: 7/10 for the read-only observation and private archival workflow.**
The unattended mainnet execution goal is not ready for release.

1. Sequential REST observations remain non-atomic. Stable endpoints cannot rule
   out a change-and-revert between reads or later exit cancellation. Coverage
   does not prove execution quality, intended stop placement or guaranteed fills.
   The extra position reads increase request cost and can exhaust the freshness
   budget on a slow or much larger DEX set; that produces an error, not omission.
2. Archives are not an installed collector, private backup or alert service.
   Failed/canceled local processes can stop producing files. Full fill/funding/
   transfer pagination and cash-flow reconciliation are still separate work.
   A sync failure can leave a completed file without a receipt; a crash can
   leave a private temporary file. Preserve evidence and inspect outcomes.
3. Local privacy checks rely on OS ownership/mode and trusted same-user access;
   they do not audit ACLs or prevent a privileged process from editing data.
   Push hooks can be bypassed, an allowlisted repository can change visibility,
   and filename rules cannot recognize all private content copied elsewhere.
   Independently review the exact filtered export before any publication.

The experimental owner-wallet helper is not part of these commits or the
released trading engine. Its successful manual use does not establish reviewed
agent custody, durable entry recovery, monitoring or mainnet release evidence.

## Top three improvements

1. Build a durable account collector with overlapping paginated fills, funding
   and transfers; reconcile cash contributions separately from trading returns,
   preserve source timestamps and report unexplained residuals.
2. Wire protection/staleness incidents into the existing notification outbox and
   prove delivery plus independently detected collector failure. Define backup
   retention for private evidence. Neither scheduling nor real delivery is
   installed by this change.
3. Assemble the scoped local agent signer, immutable batch approval, durable
   nonce ownership and existing bracket/recovery lifecycle. Exercise partial
   fills, lost replies, restarts and revocation on credentialed testnet, then
   obtain independent review for a separately scoped mainnet release.
