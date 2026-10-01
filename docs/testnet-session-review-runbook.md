# Independent testnet session review runbook

For the **reviewer**, not the operator. If you ran the session, you cannot
review it; hand this document to someone who did not.

Your output is a `ReviewAttestation` file. Without one, a recorded session is a
candidate and does not count toward the three sessions the release gate
requires. Three candidates and zero attestations is zero sessions.

## What you are actually being asked to certify

Read this before running anything, because the tool cannot do the part that
matters.

`hypercarry-testnet-operator review` performs **no network I/O**. It verifies
that a set of files are internally consistent: that digests match the immutable
evidence and the manifest, that the journal replays to the claimed final state,
that the final-state artifacts agree with the journal, and that nothing changed
underneath it mid-validation. That is real and worth having.

What it cannot do is establish that the final-state files describe reality. They
were fetched by whoever assembled the session artifacts. A session in which the
operator fabricated all three would pass every check the tool makes, provided
the fabrications were mutually consistent.

**You supply the independence.** You do that by querying the venue yourself
(step 3) and comparing against the artifacts you were handed. The acknowledgement
you type is your assertion that you did. The tool records
`independent_review: true` because you said so, not because it proved anything —
so do not type it until step 3 and step 4 have actually been done.

## 1. Receive the artifacts

Ask the operator for the session directory. For session
`testnet-btc-<timestamp>-<short-commit>` it contains:

| File | What it is |
|---|---|
| `session.evidence.json` | Immutable harness evidence |
| `operator-run.json` | Exact operator configuration |
| `session.journal.jsonl` | Durable lifecycle journal |
| `manifest.json` | SHA-256 of every artifact |
| `independent-final-open-orders.json` | Account-wide open orders at close |
| `independent-final-order-status.json` | Terminal status of the session order |
| `independent-final-user-fills.json` | User fills at close |
| `hypercarry-testnet-operator` | The operator executable that ran |
| `hypercarry-foundry-testnet-signer` | The signer executable that ran |

Take your own copy and work from it. Do not review files the operator can still
write to.

Also ask for, and record: the source commit, the UTC session interval, the
testnet account address, and the venue order ID.

## 2. Manual secret scan

The harness's automated scan already ran. You are asserting a **human** looked.
Read every artifact — they are small. You are looking for anything that could
authenticate as the account: a private key, seed phrase, mnemonic, password,
keystore blob, or a bare 32-byte `0x` value that is not an order or transaction
identifier.

Check the journal and the config especially; those are the files that carry
operator-supplied strings. Confirm the session directory and its contents are
owner-only (`ls -l`), and confirm nothing was copied into a shared location.

A single leaked secret fails the session. Do not proceed.

## 3. Independently re-query the venue

This is the step that makes the review independent. Query the official testnet
API yourself, from your own machine, and compare against the artifacts:

```sh
API=https://api.hyperliquid-testnet.xyz/info
ACCOUNT=0x...        # from the operator
OID=...              # venue order ID from the evidence

curl -s "$API" -H 'Content-Type: application/json' \
  -d "{\"type\":\"openOrders\",\"user\":\"$ACCOUNT\"}" | jq .

curl -s "$API" -H 'Content-Type: application/json' \
  -d "{\"type\":\"orderStatus\",\"user\":\"$ACCOUNT\",\"oid\":$OID}" | jq .

curl -s "$API" -H 'Content-Type: application/json' \
  -d "{\"type\":\"userFills\",\"user\":\"$ACCOUNT\"}" | jq .
```

Compare each against the corresponding `independent-final-*.json`. They will not
be byte-identical — time has passed, and the account may have been used since —
but the substance must agree:

- **Open orders**: the session's order must not be open. Account-wide open
  orders attributable to this session: zero.
- **Order status**: terminal, and the same terminal state the evidence claims.
- **User fills**: fills matching the session's order ID must match the claimed
  cumulative filled quantity — including zero, if the session claims zero.

If the venue no longer serves history that far back, say so in your notes and
treat the final-state check as unverified rather than passed. An unverifiable
check is not a passed check.

## 4. Replay the journal yourself

Read `session.journal.jsonl`. It is one JSON object per line, in sequence. Walk
it and satisfy yourself that:

- the lifecycle transitions are possible in order — no jump from submitted to
  cancelled without an intervening observation;
- the exact action recorded is the order the config authorized, and no other
  order appears;
- every submission reaches a resolved outcome;
- no risk rejection is followed by a submission of the same order;
- the final state matches what the evidence claims and what you saw in step 3.

The tool checks this too. Do it anyway — the tool checks the journal against the
evidence, and you are the only one checking both against what the venue says.

## 5. Run the review command

Only now. It writes a new file and refuses to overwrite an existing one, so
choose a path that does not exist.

```sh
hypercarry-testnet-operator review \
  --evidence            ./session.evidence.json \
  --config              ./operator-run.json \
  --journal             ./session.journal.jsonl \
  --manifest            ./manifest.json \
  --final-open-orders   ./independent-final-open-orders.json \
  --final-order-status  ./independent-final-order-status.json \
  --final-user-fills    ./independent-final-user-fills.json \
  --output              ./review-<your-identifier>.json \
  --reviewer            "<your bounded, non-secret identity>" \
  --acknowledgement     "I INDEPENDENTLY REVIEWED THIS HYPERCARRY TESTNET SESSION AND FOUND NO SECRETS OR UNMANAGED ORDERS"
```

The acknowledgement must be exact. `--reviewer` is recorded in the attestation
and becomes part of the permanent evidence, so use an identity that is
meaningful to a later auditor and is not itself a secret.

The command refuses a session that already carries a reviewer or a completed
manual scan, so it cannot be used to attest the same session twice.

## 6. Report

Return to the operator:

- the attestation file and its SHA-256;
- your step-3 comparison, including anything that differed;
- anything you could not verify, stated as unverified rather than omitted.

A review that found a problem is a successful review. Say what you found.

## What a passing review does and does not establish

It establishes that one session's artifacts are internally consistent, that its
journal replays cleanly to a terminal state, that a human read the artifacts for
secrets, and that a human compared the claimed final state against the venue.

It does not establish that the executor is safe to run on mainnet. The release
gate requires **three** such sessions, and separately requires that the fault
exercises — disconnect and reconnect, uncertain submission, restart, stale data,
throttling, fill/cancel races, and kill-switch activation — are covered across
them. The `faults_reviewed` field in your attestation is copied from the
session's `faults_injected`. A session that injected no faults contributes no
fault coverage, however clean it is.

The 2026-09-01 candidate is one such session: clean, and with no faults injected.
Reviewing it successfully makes it session one of three, and leaves the entire
fault matrix still to be covered by the other two.
