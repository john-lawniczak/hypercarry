# Post-implementation audit — 2026-11-03

Scope: all seven November 3 implementation commits, `4e54f9b` through `d0b1051`,
against `c643be9`, including nearby executor, signer and recovery code. This
review focused on actual failure paths, configuration boundaries, concurrency,
cryptographic input validation, packaging and the strength of the tests. The
fixes below are applied; no account was funded, infrastructure provisioned or
order submitted. This review does not replace the required independent signing
and deployment security review.

## Findings and applied fixes

### 1. High: provider stdin could bypass the signing deadline

In [the provider adapter](../crates/hypercarry-mainnet-services/src/provider.rs),
the supervising thread serialized the request into a blocking pipe before
starting the timeout. A provider that never reads stdin, combined with a long
configured key ID, could fill the pipe and block the signer despite the configured
deadline. The same signer serves emergency actions, so this is a recovery
availability issue, not merely slow diagnostics.

Fix: start timing before input delivery and write on a worker while the supervising
thread enforces stop/deadline. Require successful input completion before accepting
output. A stalled writer is released when helper-group cleanup closes the pipe.
Regression: a non-reading helper with a 60 KB request and a 100 ms deadline must
return an error within two seconds. Its reserved nonce remains consumed.

### 2. Medium: completed helpers could leave live descendants

Provider cleanup only covered selected failures. The operations probe cleaned up
on errors but not success. A helper could fork a background writer, close its
output and exit; the invocation would finish while the descendant retained the
service identity and kept running. Parsing, pipe-read and wait errors also did
not share one cleanup guarantee.

Fix: [a lifetime-bound helper group](../crates/hypercarry-supervisor/src/process.rs)
kills the process group and reaps the direct child on every return path. Provider
and probe callers both use it. Tests cover timeout, successful exit and refusal
with delayed descendant writes; no marker may appear afterward. A malicious
helper that deliberately leaves its process group remains an OS-containment
concern. Executable pinning is not a sandbox, and the AWS CLI continues to inherit
the outer provider group so killing the provider also reaches it.

### 3. High: path separation omitted some authority inputs and derived locks

[Executor validation](../crates/hypercarry-executor/src/config.rs) checked its
configuration against mutable runtime paths but did not include release evidence
or MCP approval/consumed paths. For example, an evidence path equal to the executor
lease could be read successfully and then replaced when the lease starts. The
service validators also omitted the derived supervisor lock; on a shared filesystem,
atomic state replacement at that path could detach a live lock from its filename.
The doctor did not compare probe inputs against all service state paths.

Fix: validate evidence, approval and consumed paths before confirmation, claiming
consent or starting runtime writers. Include the supervisor lock in executor and
service checks, and probe inputs in the doctor's combined path set. Existing-ancestor
resolution and inode checks remain in use. Tests reject absent lease/lock collisions,
hard-linked authority inputs, signer/watchdog lock collisions and a probe executable
configured as signer state. They also check that retained evidence is unchanged.

### 4. Medium: diagnostic sampling bypassed the daemon's writer lock

The `sample` command could replace a running daemon's health file with its
diagnostic-only NOT READY observation. This could interrupt a valid canary at a
readiness check and produce intermittent failures during routine diagnosis.

Fix: [both commands](../crates/hypercarry-supervisor/src/main.rs) acquire the same
non-truncating exclusive lock before publication. A regression checks second-writer
refusal, retained health contents and successful lock acquisition after release.

### 5. Medium: receipt freshness was checked before expensive artifact I/O

The [operations probe](../crates/hypercarry-supervisor/src/operations.rs) captured
the clock before reading receipts and hashing the recovery artifact. A slow disk
or large artifact could consume a short receipt lifetime, yet return an affirmative
receipt based on the earlier clock. The supervisor's health age does not substitute
for independently configured receipt age limits.

Fix: obtain the clock and check both receipt ages after artifact verification.
The clock is injected for deterministic tests. Stale/future receipt tests remain,
and regressions cover clock failure and require artifact verification to precede
the final clock read. Parent probe timeouts still bound the entire invocation.

### 6. Medium: the KMS public-key helper accepted off-curve coordinates

The [KMS helper](../crates/hypercarry-mainnet-services/src/bin/kms_provider.rs)
checked the secp256k1 DER prefix and length, then hashed arbitrary coordinates.
A malformed public-key file could therefore produce a plausible Ethereum address
that no corresponding KMS key could sign for. Later signature recovery would refuse
it, but the helper could misdirect agent provisioning.

Fix: decode and validate the SEC1 point with the current k256 verifier before
deriving the address. The regression checks the known public fixture and refuses
all-zero/off-curve and out-of-range coordinates under an otherwise valid SPKI prefix.
The helper still does not prove live KMS access or venue agent authorization.

### 7. Medium: an incomplete bundle could report a successful role installation

[The installer](../tools/release_bundle.py) verified every listed file but did not
require that a role's binaries and units were listed. A checksummed, internally
consistent archive without a supervisor or watchdog unit could report `installed`
success, leaving a deployment with no corresponding service.

Fix: require the shared core executable set with executable mode (the doctor
checks both roles' artifacts) and the selected role's required unit files before making installation changes. The AWS reference recovery role
includes the shipped KMS adapter. Tests remove an executor binary or recovery unit
from an otherwise valid archive and require failure with an untouched destination.
Verification/staging remain usable for inspecting archives independently of installation.

### 8. Medium: every observation discarded the HTTP connection pool

The daemon constructed a new blocking reqwest client on every cycle. That starts
a runtime thread, loads TLS configuration and discards reusable connections at the
end of each all-DEX batch. It increases resource use and latency precisely where
health freshness is tightly bounded.

Fix: construct one client for the daemon lifetime and borrow it for each observation.
Concurrent DEX queries share that pool; diagnostic sampling constructs one client
for its single batch. Request/response limits and redirect policy are unchanged.
This is a structural correction; deployment latency and venue request-budget
measurements remain necessary and have not been inferred from offline tests.

### 9. Medium: readiness failures discarded actionable diagnostic context

Journal and operations probe errors were collapsed to booleans, private-stream
errors were discarded, and observation errors only said that health was withdrawn.
Operators could not distinguish corrupted receipts, failed durability checks or a
subscription/schema failure from an outage without instrumenting the code.

Fix: emit the stage and a bounded, escaped error message on stderr for these
failures. Do not dump helper stdout/stderr or full venue response bodies. Readiness
still fails closed. Diagnostics are context for repair, never authority to override
the health result.

## Ship assessment and remaining risks

**Ship rating: 6/10 for a funded mainnet release.** The fixes make the gated source
candidate more robust; they do not establish production custody, independent
recovery or security-review evidence. Mainnet release remains closed.

The most important remaining risks are:

1. Real KMS/IAM access, protected socket forwarding, timestamp-preserving file
   replication and the trusted monitoring receipt writer have not been exercised
   together on the intended Linux hosts. A locally passing doctor cannot prove them.
2. The complete supervisor loop lacks a deterministic end-to-end fault harness
   combining REST, WebSocket, journal writes, publication, signals and reconnects.
   Existing tests establish components and several races, not every interleaving.
   All-DEX batch latency and request cost under actual venue load remain uncertain.
3. The signing migration still needs independent review. Recovery intentionally
   attempts a bounded reduce-only IOC and cannot guarantee flattening during a
   venue/signing outage or insufficient liquidity. The account needs a tested
   independent manual recovery route.

The three highest-leverage follow-ups are:

1. Build the full supervisor/executor fault harness with controlled HTTP/WS
   endpoints and clocks; cover event-during-publication, stale replication, partial
   journal writes, cancellation and reconnect. Keep production endpoints fixed.
2. Provision the AWS reference topology, integrate actual delivery/recovery receipts,
   and retain measured outage/restart/partial-fill exercises plus three independently
   reviewed credentialed testnet sessions.
3. Obtain independent signing/custody/recovery review, then build and approve the
   final public-source Linux artifacts with exact hashes and a witnessed rollback drill.

## Verification

The final verification record is maintained in [development status](../DEV_STATUS.md).
Required checks are workspace tests, strict Clippy, formatting, default-off and
Rust 1.94.1 compilation, the Python release/export suite, dependency/supply-chain
checks, official SDK signing vectors and a clean-source release build. Any earlier
bundle built during implementation is superseded by the final audited commit;
public release identity must still come from the final public export.
