# Testnet operator and external signer protocol v1

`hypercarry-testnet-operator` is a non-published, testnet-only binary for
producing the first credentialed execution evidence. It is not the future
mainnet executor and is not reachable from the default `hypercarry` CLI.

The operator can place one exact GTC limit order, cancel it by durable `cloid`,
REST-reconcile to a terminal state, independently query all account open
orders, and atomically write secret-free evidence. Evidence binds hashes of the
strict config, durable journal, and exact operator executable. It cannot select
mainnet, load a key, start a signer process, overwrite evidence, or submit a
second order.

## External prerequisite

The repository cannot safely create or custody an operator's wallet. Before a
credentialed run, the human operator must:

1. Create a dedicated Hyperliquid testnet API/agent wallet and authorize it for
   a dedicated testnet trading account.
2. Fund only that account with faucet USDC.
3. Put the agent key in a separately reviewed signer process backed by an HSM,
   OS keychain, or equivalent protected store.
4. Run that signer on a Unix-domain socket inside an absolute, owner-only
   directory. The operator rejects a symlinked socket or a socket directory
   accessible to group/other users.
5. Independently obtain fresh market metadata and account/risk state for the
   one configured market. Never infer an asset ID, tick, size step, equity, or
   open-order count from the example file.

Do not put a private key, seed phrase, signer response, or signature in the
operator config, environment, command line, journal, evidence, issue, or log.
The JSON config rejects unknown fields so secret-looking convenience fields
cannot silently enter its schema.

## Signer protocol

The operator opens one new local socket connection per signature and writes one
newline-terminated JSON object:

```json
{
  "schema_version": 1,
  "network": "testnet",
  "key_id": "keychain/hypercarry-testnet-agent",
  "signer_address": "0x...",
  "nonce": 1788100000000,
  "expires_after": 1788100005000,
  "action": {"type": "order", "orders": [], "grouping": "na"}
}
```

The provider must use Hyperliquid's testnet L1 signing domain and return one
bounded JSON response before closing the connection:

```json
{
  "schema_version": 1,
  "network": "testnet",
  "key_id": "keychain/hypercarry-testnet-agent",
  "signer_address": "0x...",
  "signed_request": {
    "action": {},
    "nonce": 1788100000000,
    "signature": {"r": "0x...", "s": "0x...", "v": 27},
    "vaultAddress": null,
    "expiresAfter": 1788100005000
  }
}
```

The operator pins the response network, alias, and address. The execution
library then requires the signed request to preserve the exact action, nonce,
expiry, null vault, and bounded signature shape before `/exchange` receives it.
Both `r` and `s` must be `0x` plus exactly 64 hexadecimal digits, including
leading zeroes; `v` must fit in one byte. Do not rely on a generic U256 JSON
serializer because some emit minimal-width scalars.
The venue independently rejects a signature from an unauthorized wallet.

Socket reads and writes have configured timeouts. Responses above 64 KiB,
unknown response fields, malformed JSON, identity changes, and unavailable
sockets fail closed without including provider output in an error.

## Configure and preflight

### Optional Foundry-keystore provider

The repository includes a non-published, testnet-only provider for operators
who keep the agent in an encrypted Foundry keystore. It is a separate process:
the operator still cannot read a key or password. The provider validates one
bounded order followed by its exact `cloid` cancel, asks a pinned absolute
`cast` executable to sign each prehash, verifies the recovered agent address,
and then exits. `cast` prompts in the provider's terminal for every signature;
never pass a password through an argument, file, or environment variable.

The keystore and socket directory must be accessible only to their owner. In a
dedicated terminal, replace the paths and live asset/notional policy as needed:

```sh
cargo run -p hypercarry-testnet-operator \
  --bin hypercarry-foundry-testnet-signer --locked -- \
  --socket /absolute/private/directory/hypercarry-testnet-signer.sock \
  --keystore /absolute/path/to/foundry/keystores/hypercarry-testnet-agent \
  --cast-bin /absolute/path/to/cast \
  --key-id foundry/hypercarry-testnet-agent \
  --signer-address 0x2222222222222222222222222222222222222222 \
  --allowed-asset 3 \
  --allowed-side buy \
  --max-notional 12
```

The sample asset ID is illustrative and must be replaced with the current
testnet metadata value. The signer rejects mainnet, extra or mutated fields,
multiple orders/cancels, mismatched identity, stale or excessive TTLs, a wrong
asset/side, excess notional, and a cancel that does not match the signed order.
It does not submit requests or transfer collateral.

The provider is deliberately one-shot and advances after returning a valid
signature. If the operator rejects that response locally or exits before
submission, stop the provider with Ctrl-C and do not reuse its socket or phase.
The provider handles Ctrl-C and termination signals while waiting for the next
connection or for interactive `cast`, terminates and reaps an in-flight `cast`
child, exits without advancing its phase, and removes the socket. Confirm the
socket is absent before starting a new provider. Start every retry with a new
signer process and new session/correlation IDs, journal, config timestamps, and
evidence path. Preserve the failed journal for diagnosis, and independently
require zero account open orders before retrying.

Copy `docs/testnet-operator-config.example.json` outside the repository and
replace every placeholder with independently verified, current values. The
example's timestamp and market/account values are intentionally unusable.
Set `risk.account_mode` to `unified_account` only when the official
`userAbstraction` response is `unifiedAccount`; use `standard` only when it is
`disabled`. Immediately before signing, `run` verifies that mode, available
USDC from the mode-appropriate account endpoint, account-wide open-order count,
and the agent-to-master authorization. Unified accounts source available USDC
from `spotClearinghouseState`; their individual perpetual state is not a valid
collateral source.
Use a new session ID, correlation ID, journal, and evidence path for every run.

Preflight performs no network I/O and never invokes the signer:

```sh
cargo run -p hypercarry-testnet-operator \
  --bin hypercarry-testnet-operator --locked -- \
  preflight --config /absolute/private/directory/operator.json
```

It validates typed testnet identity, strict schema, addresses, exact market
allowlist, quantization, all risk limits, snapshot freshness, bounded timeouts,
absolute non-symlink paths, signer-socket permissions, kill-switch state, and a
non-overwriting evidence destination.

## Execute one lifecycle

Review the preflight output, independently confirm the API-wallet authorization
and zero unmanaged orders, then enter the exact acknowledgement:

```sh
cargo run -p hypercarry-testnet-operator \
  --bin hypercarry-testnet-operator --locked -- \
  run \
  --config /absolute/private/directory/operator.json \
  --acknowledgement \
  "I ACKNOWLEDGE HYPERCARRY TESTNET EXECUTION AND EXTERNAL EFFECTS"
```

The binary re-runs every local preflight boundary, verifies live account mode,
collateral, open orders, and agent authorization, opens the exclusively locked
journal, evaluates the comprehensive risk policy, places at most one order,
reconciles uncertain placement before cancellation, cancels by `cloid`, polls
to a terminal state, queries account-wide open orders through a separate final
request, closes the journal lock, hashes it, and persists evidence without
overwrite.

An outcome is lifecycle-clean only when the order is `cancelled` or `filled`,
there is no unresolved submission, and the dedicated account has zero open
orders. Evidence deliberately records `manual_secret_scan_completed: false`,
an empty fault list, and no reviewer. The immutable harness evidence is always
a candidate; do not edit it to add review claims.

## Independently review a candidate

A human who did not operate the session must first inspect the source revision,
manually scan every retained artifact for credentials or signatures, and verify
that the retained independent final-state files came from the official API. The
reviewer then runs the offline `review` subcommand with those original
manifest-bound, owner-only files:

```sh
cargo run -p hypercarry-testnet-operator \
  --bin hypercarry-testnet-operator --locked -- \
  review \
  --evidence /absolute/private/session/session.evidence.json \
  --config /absolute/private/session/operator-run.json \
  --journal /absolute/private/session/session.journal.jsonl \
  --manifest /absolute/private/session/artifact-sha256.txt \
  --final-open-orders /absolute/private/session/independent-final-open-orders.json \
  --final-order-status /absolute/private/session/independent-final-order-status.json \
  --final-user-fills /absolute/private/session/independent-final-user-fills.json \
  --output /absolute/private/session/review-attestation.json \
  --reviewer independent-reviewer-id \
  --acknowledgement \
  "I INDEPENDENTLY REVIEWED THIS HYPERCARRY TESTNET SESSION AND FOUND NO SECRETS OR UNMANAGED ORDERS"
```

`review` performs no network or signing I/O. It fails closed unless every input
is an absolute, bounded, owner-only regular file in an owner-only non-symlinked
directory and the output does not exist. It strictly decodes the original
unreviewed evidence, requires a clean terminal testnet lifecycle with zero
exceptions, verifies the config/journal/executable and independent-check hashes
against the manifest, and replays exactly one allowed testnet action from the
journal. It also requires an empty account-wide open-order response, a matching
official terminal order record, and matching user fills whose total equals the
recorded cumulative fill. Every input is hashed again after validation, so a
file changed during review fails before any attestation is written.

After those checks, the command atomically creates an owner-only schema-v1
`review-attestation.json` and prints its SHA-256 digest. The attestation binds
the session interval, source revision, reviewer identity, order identity and
terminal state, release-gate exception counts, executable hashes, original
evidence, and all independent final-state files. It never modifies or
overwrites the harness evidence or an earlier attestation. The acknowledgement
is the human's assertion that the manual secret scan and organizationally
independent review occurred; software cannot prove that separation.

The attestation supplies the fields needed to construct one
`TestnetSessionEvidence` entry, but it does not modify the mainnet release
evidence automatically. A later release reviewer must retain the attestation
and its hash in the audit set while mapping its reviewed values into release
evidence. Fault-injection coverage also remains separate: the current clean
candidate records no injected faults.

This first harness uses REST reconciliation. The required three-session gate
still includes private-stream observation, disconnect/restart exercises, all
fault cases in the operator runbook, and independent final-state review. No
credentialed run has occurred merely because this binary compiles or preflight
passes.

## Recorded failed-closed readiness exercise

On 2026-08-31, the live account-mode, unified collateral, zero-open-orders,
agent authorization, signer socket, and offline preflight gates passed. The
encrypted Foundry provider produced a bounded order signature, but the operator
rejected its minimally encoded leading-zero scalar before `/exchange`. An
independent follow-up query returned zero open orders. Commit `0b665ca` changed
the provider to fixed-width 32-byte `r` and `s` encoding and added a regression
test. This exercise produced no lifecycle evidence and does not count toward
the three required clean credentialed sessions.

On 2026-09-01, a fresh session at commit `df1f911` passed offline and live
account gates, then timed out at the signer boundary while its interactive
`cast` child remained active. The journal contains only an allowed risk decision
and the exact action; no signed or submitted state was recorded. An independent
official query returned zero open orders. The child and signer were stopped and
the socket was removed. The signer now cancels and reaps `cast` when interrupted
instead of handling signals only in its connection-accept loop.

A later 2026-09-01 session at commit `e11e7ff` signed and submitted one bounded
order and its exact cancel, then failed closed during REST reconciliation because
the official `orderStatus` response wrapped the terminal state in a nested
`{"status":"order"}` envelope. Independent queries by both venue order ID and
`cloid` returned `canceled`, account-wide open orders were zero, and matching
fills were empty. The session produced no evidence and does not count toward the
three clean sessions. The parser now accepts that recorded nested response shape.

Commit `34644b0` subsequently produced the first clean credentialed candidate,
`testnet-btc-20260901T160209Z-f8a0d9b9`, with a cancelled terminal state, zero
fills, zero open orders, and zero unresolved submissions or policy bypasses.
Its immutable artifact hashes are recorded in `testnet-execution-evidence.md`.
It remains uncounted until an independent human runs the review workflow above.
