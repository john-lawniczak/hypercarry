# Hyperliquid testnet operator runbook

This runbook applies to a host application integrating the feature-gated
`hypercarry-execution` library or the repository's non-shipping
`hypercarry-testnet-operator` evidence harness. Hypercarry's shipped CLI remains
read-only.

## Credentials and funding

See the user-facing [testnet funding guide](testnet-funding.md) for the
master-wallet/agent-wallet separation, mainnet deposit prerequisite, faucet
procedure, and common error messages.

1. Create a dedicated Hyperliquid **testnet** API/agent wallet. Do not reuse a
   mainnet key or the master wallet.
2. Store the key only in an external signing provider (HSM, OS keychain-backed
   service, or a separately reviewed signing process). Configure hypercarry
   with only the provider's non-secret key alias, API-wallet signing address,
   and public trading-account address.
3. Confirm the provider reports typed network `testnet`; construction fails on
   a mismatch. Independently confirm that the API wallet is authorized for the
   configured trading account. Never paste a key into JSON, an environment
   variable, a command, a journal, an issue, or logs.
4. Fund the account with mock USDC through the official testnet faucet and
   verify the public account address—not the API wallet address—is used for
   account queries.

The repository cannot perform these credential steps for the operator and must
never receive the wallet secret. For the supplied harness, expose the protected
signer through the owner-only local protocol in
`docs/testnet-operator-v1.md`, then copy and replace every placeholder in
`docs/testnet-operator-config.example.json` outside the repository.
An optional Foundry-keystore provider is documented there; it delegates
interactive signing to `cast` and never accepts a raw key or password.

## Startup

1. Build the host with `--features hypersdk-signer` when embedding the pinned SDK
   adapter, or build `hypercarry-testnet-operator` for the local-socket signer
   boundary. Verify the endpoint in the build is
   `api.hyperliquid-testnet.xyz`. Hypercarry provides no raw-key loader.
2. Set a single exact market allowlist and conservative notional/leverage/loss
   limits. Ensure market metadata, account equity, open orders, rolling PnL, and
   the quote timestamp are fresh. Query `userAbstraction` first: unified
   accounts source available USDC from `spotClearinghouseState`, while standard
   accounts use `clearinghouseState`. An empty individual Perps state does not
   mean zero collateral for a unified account.
3. Check that the kill-switch path is readable and absent. An unreadable path
   fails closed.
4. Open the durable journal. If another process owns its exclusive lock, stop;
   never start a second writer.
5. Replay every nonterminal intent from the journal, query `orderStatus` by
   `cloid`, and compare against account open orders. Stop on any missing,
   unmatched, or impossible order.
6. Connect the dedicated private stream and subscribe to `orderUpdates` and
   `userFills`. After each disconnect, reconnect and repeat REST reconciliation
   before allowing another placement.
7. Enter the exact operator acknowledgement. Start with one post-only-style
   resting limit at minimum test size where practical; M8 currently emits GTC,
   so price selection must remain inside the configured deviation policy.

For the supplied harness, run its offline `preflight` command first and inspect
the secret-free JSON summary. Its `run` command rechecks preflight and accepts
only `I ACKNOWLEDGE HYPERCARRY TESTNET EXECUTION AND EXTERNAL EFFECTS`. The
harness performs one REST-reconciled lifecycle; it does not replace the private
stream and fault exercises required below.

The Foundry provider is one-shot. If a signature is produced but the operator
fails locally before submission, stop that signer and confirm its socket is
removed. Ctrl-C and termination signals are handled while it waits for the next
connection or an interactive `cast` child; shutdown cancels and reaps the child,
then removes the socket without advancing the signer phase. Independently query
account-wide open orders, preserve the failed
journal, and retry only with a new signer process, fresh market/account data,
new session and correlation IDs, and new journal/evidence paths. Signature
scalars must retain leading zeroes as exact 32-byte hex values; commit
`0b665ca` adds this boundary and regression coverage.

## Normal shutdown

1. Engage the filesystem kill switch to stop new risk approvals. Cancellation
   remains available while the switch is engaged.
2. Cancel every nonterminal order by `cloid`.
3. Continue private events and REST reconciliation until every order is
   `filled`, `cancelled`, or `rejected`, with no uncertain submissions.
4. Query account open orders independently and require zero unmanaged orders.
5. Close the private stream, flush/sync the journal, then release its lock.
6. Record session start/end, commit, account, limits, fault observations, final
   states, and the independent zero-open-order check without signatures or
   credentials.

## Uncertain submission or disconnect recovery

- Do not create a new intent and do not change the `cloid`.
- Query `orderStatus` using the public account and original `cloid`.
- If found, apply its state and resume the private stream. If not yet found,
  keep `submission_uncertain`, wait within the bounded policy, and query again.
- If uncertainty cannot be resolved, keep the kill switch engaged and escalate;
  never infer that a timeout means rejection.
- After process restart, replay lifecycle transitions and private-event IDs from
  the journal before consuming snapshot fills.

## Emergency cancellation

1. Create the configured kill-switch file from a separate terminal/process.
2. Confirm new placements receive `kill_switch` rejection and the signer is not
   invoked.
3. Cancel every known `open`, `partially_filled`, `cancel_pending`, or uncertain
   order by original `cloid`; a fill may still win the race.
4. REST-reconcile until terminal, then independently query all account open
   orders. If any order is not represented in the journal, treat it as unmanaged
   and cancel it through a separately reviewed venue tool.
5. Preserve the journal and incident timeline. Do not remove the kill-switch
   file until root cause, reconciliation, and restart checks are complete.

## Required pre-mainnet faults

Repeat deterministic and live testnet sessions with stale quotes, delayed HTTP
responses, dropped private frames/disconnects, HTTP 429, uncertain submissions,
duplicate snapshots, fill/cancel races, restart during each nonterminal state,
and kill-switch activation. M9 remains closed until the evidence record shows
zero orphaned/unmanaged orders, unresolved submissions, policy bypasses, and
secret leakage.
