# Attended local testnet session

This handoff uses the existing encrypted **testnet agent**, not an owner key. The
helper prepares a fresh plan and optionally starts the signer; it never submits
an order. The operator reviews the displayed policy SHA-256 and enters the
keystore password locally. No password, raw key or signature belongs in chat.

From the repository root, build the pinned binaries and run offline checks:

```sh
uv run --offline --no-project --with hyperliquid-python-sdk==0.24.0 python tools/check_local_operator.py
cargo build --locked -p hypercarry-notifications --features delivery --bin hypercarry-notify -p hypercarry-testnet-operator --bin hypercarry-testnet-recovery
```

Use a short absolute private directory for the Unix socket. All sessions for the
account share its `operations` and `signer` directories; never delete them to
clear a reservation or reset a nonce budget. Keep other account writers stopped.

## Freeze the qualifying candidate

After the implementation commit and offline checks, freeze clean source and
release binaries. Keep the output under an ignored private directory:

```sh
uv run --offline --no-project --with hyperliquid-python-sdk==0.24.0 \
  python -m tools.local_operator.mainnet freeze-candidate \
  --binary-dir "$PWD/target/release" \
  --output "$PWD/data/private-reviews/local-bracket/candidate-$(git rev-parse --short HEAD).json"
```

This builds the locked release artifacts and prints the candidate SHA-256; it
does not sign, unlock, or submit anything. Retain test and advisory/scope-check
receipts with it. Pass both `--candidate /absolute/path/to/local-bracket/candidate-COMMIT.json`
and `--candidate-sha256 PRINTED_HASH` to every qualifying invocation below,
including closes. The helper selects the frozen binary paths. Keep one source
commit and binary set through full, partial and fault/restart sessions. Rebuilding
changed artifacts or changing source invalidates that candidate. Older sessions
and invocations without these flags are practice evidence, not this qualification.

The operator's next step is to start the helper below in their own terminal,
approve its displayed testnet policy digest and unlock the testnet agent locally.
After the signer reports ready, run its printed execute command once in a second
terminal. Complete and review one scenario at a time before the next.

```sh
uv run --offline --no-project --with hyperliquid-python-sdk==0.24.0 python tools/attended_testnet.py \
  --account PUBLIC_TRADING_ADDRESS --agent PUBLIC_AGENT_ADDRESS \
  --keystore /absolute/path/to/encrypted-testnet-agent \
  --directory /private/tmp/hc-testnet --serve
```

Default proposed exercise: native BTC long, $15 notional budget, isolated 1x,
minimum $950 free-USDC reserve, TP about 3% above the checked price, SL about 1%
below it, entry slippage bound 0.5%. All prices and actual rounded quantity are
shown before signing. These are testnet exercise parameters, not an investment
recommendation. The helper permits only $12–$20 notional and at least $950 reserve.
A changed book can refuse execution; prepare a fresh plan instead of editing it.

The helper requires a clean committed source tree, saves an immutable plan and
policy, and prints the exact second-terminal `execute` command. The plan expires
five minutes after preparation; unlocking does not restart that clock.
The displayed `action_ttl_ms: 30000` separately authorizes each signed action
for at most 30 seconds, capped by session expiry. Fresh checks still run after
signing, and submission requires at least five seconds left for venue receipt.

With `--serve`, the final JSON block contains `"approval"` and then `"sha256"`.
Review the policy, then copy the **64-character value of that final `"sha256"`**
into the `Type the policy SHA-256 to approve this testnet session:` prompt and
press Enter. Paste only the value, without quotes: do not type the words
`SHA-256`, the policy JSON, the earlier plan hash, or `plan_digest`. The next
prompt asks for the keystore password locally.

An incorrect approval ends the helper before unlock. Rerun the helper to prepare
a fresh plan and use its newly displayed policy hash. If the plan expires, stop
the signer with Ctrl-C and prepare a fresh plan; do not reuse an old hash.

Once the signer reports ready, copy the printed command intact into a second
terminal; the module and subcommand are separate words:
`python -m tools.local_operator execute`. Run the command once, retain
the result, and reconcile any uncertainty before another plan. Never auto-retry.
`execute_in_second_terminal` is the JSON field name, not a shell command; its
value is the full command beginning with `uv run`. When working with Codex,
leave the signer running and report ready so Codex can run that command.

An explicit `Action already expired` venue response is retained with its intent
and broadcast record. Reconciliation can finalize that rejected attempt only
after signature expiry, fresh target flatness/no orders, and `unknownOid` for
all three original client IDs. A missing response after broadcast remains
uncertain. Reconciliation never resubmits; any new entry needs a fresh plan.

For a full close, stop the entry signer with Ctrl-C, then rerun the helper with
`--close-adoption /absolute/operations/ENTRY_SESSION_ID/position.json --serve`.
The plan uses current exposure and reviewed exit IDs; it requires a new local
policy approval. `--quantity` permits an explicit partial close. Inspect and
record protection for the residual before preparing its full close. After a
partial close, obtain a fresh adoption/ownership record; never edit the original
record to make an observation match. The close path must independently confirm
terminal status, flatness and owned-exit cleanup.

Partial plans below 10 USDC are rejected before signing. Execution preflight
and post-signing checks repeat the rule using fresh residual size. Review rounded
quantity × approved IOC limit; half of a $15 position is below the minimum.
Only a close of the complete freshly observed residual is exempt. These local
checks do not guarantee venue acceptance. An explicit rejection requires
inspection and reconciliation, not an automatic retry or a fill claim based
solely on the process exit code.

When publishing findings during qualification, use a separate documentation
checkout. The execution checkout must stay at the frozen source commit with its
original artifact paths. Once qualification finishes, updating that checkout or
changing code requires a new candidate identity; keep prior evidence labeled with
the source that actually ran.
After any source change, freeze to a new filename and use the new digest and
artifact paths. Older launchers pin their original candidate and must not be
reused for qualification. Keep old manifests and observations privately.

Required distinct sessions and review are listed in
[validation](local-operator-validation.md). A successful normal lifecycle
is only one scenario, subject to independent review. Recovery handoff uses `plan-recovery` from
[the integration guide](local-operator-integration.md). Mainnet pilot scope is
gated separately by [the default-off mainnet adapter](local-mainnet-bracket-pilot.md).

Console notification test: run the notifier with a private console-only config,
enqueue an explicitly labeled test event, then `run --once`. Confirm the message
visually. The outbox distinguishes local display from remote delivery and human
acknowledgement; automated stdout capture does not supply human confirmation.
