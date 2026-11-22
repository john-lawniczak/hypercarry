# Experimental local operator integration

The reusable integration lives in `tools/local_operator`. It uses the official
Python SDK pinned to 0.24.0 for signing and the existing schema-v1 Rust Unix
socket request/response envelope. The normal Rust analytics CLI remains
read-only. The commands below remain testnet-only for execution. The separate
[mainnet adapter](local-mainnet-bracket-pilot.md) adds an explicit release gate;
its implementation does not approve a live pilot.

## Step 1: bounded agent signing

Run from the repository root with the cached SDK:

```sh
uv run --offline --no-project --with hyperliquid-python-sdk==0.24.0 \
  python -m tools.local_operator signer --policy /private/session.json \
  --keystore /private/agent-keystore.json --state /private/signer-state \
  --socket /private/signer-state/agent.sock
```

Use real absolute, owner-only paths (directories 0700, files 0600), with no
symlink components. The named directories above are illustrative. The signer
prints the exact policy and requires its SHA-256 before one local keystore
unlock. At the approval prompt, paste only the 64-character `"sha256"` value
following the final `"approval"` block, without quotes. The words `SHA-256`,
`plan_digest`, and the earlier plan hash are not valid approval inputs. See
[the attended runbook](attended-testnet-session.md) for preparation and restart
instructions. Account and agent must differ. Only testnet is accepted, with isolated
1x, at most one exact bracket entry, bounded reduce-only exits/closes, explicitly
approved cancel IDs, a finite action budget and an expiry of at most one hour.
It cannot sign transfers, withdrawals, arbitrary hashes or mainnet actions.

Policies may explicitly include `action_ttl_ms` between 10,000 and 30,000;
omitting it preserves the original ten-second bound. The attended helper uses
30,000 and includes it in the displayed policy hash. Actions cannot outlive the
session, and the client requires five seconds remaining after all final checks
before broadcast. This margin reduces expiry races; it cannot guarantee arrival.
The venue's [expiry contract](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint#expires-after)
rejects actions received after their signed deadline.

Entry and close preflight test fresh executable depth at the exact approved price and
size. It does not reapply slippage to a new quote and compare that derived limit
with the old one. Movement inside the approved price remains eligible only if
all fresh risk, fee, capacity and account checks also pass.

The policy fields are validated in `signer.validate_policy`; planning commands
generate this file. The policy includes a full-plan digest and reserve; executors
reject a changed plan and perform the live reserve checks. The signer does not infer agent authorization from
possession of a key: the submitting client must freshly verify the venue binding.
It uses the existing socket envelope but intentionally refuses action types
outside this profile; it is not a drop-in replacement for the old canary signer.

Persistent state consumes the per-agent nonce and session action before signing.
Changing a persisted session policy or retrying a signed entry is refused.
Never remove state to retry uncertainty or share the agent with another writer.
The socket and state are private to the current OS user; this is not isolation
from malicious software running as that same user. Python and SDK key copies
cannot be reliably erased; session expiry/process exit bounds their lifetime.
No wallet secret or signature is written to the journal or printed.

Verification uses public, unfunded fixture keys only:

```sh
uv run --offline --no-project --with hyperliquid-python-sdk==0.24.0 \
  python -m unittest tools.local_operator.test_signer tools.local_operator.test_sdk
```

Signer and SDK tests cover policy refusal, durable entry/nonce reservations, cumulative
close limits, explicit cancellation ownership, unsafe files, SDK wire ordering,
existing Rust signature vectors and encrypted-fixture unlock. Real credentials,
testnet exchange submissions and independent security review remain outstanding.

## Step 2: adoption, close and reconciliation

`python -m tools.local_operator --help` lists the commands. `status --network
mainnet|testnet --account ADDRESS` is unsigned and discovers every DEX, with
at most four concurrent independent DEX reads. Within each DEX, positions bracket
orders; stale flat states and changed discovery fail the complete observation.

`adopt --network NETWORK --account ADDRESS --coin COIN --output PRIVATE_JSON`
prints a fresh isolated-1x position and its eligible native trigger exits. Enter
the displayed digest to record explicit ownership. This writes no exchange
order, invents no historical entry and marks history incomplete. Other target
orders must be reconciled first. Mainnet adoption is observation only.

`plan-close --adoption PRIVATE_JSON --agent AGENT --key-id ALIAS --output PLAN`
prepares a testnet full close; add `--quantity DECIMAL` for a partial close and
`--slippage-bps N` (0–100, default 30). It writes a plan and sibling
`.policy.json` for the signer. Prices and displayed book depth are checked.
Partial closes also require rounded quantity × approved IOC limit ≥ 10 USDC.
A full close of the freshly observed residual remains allowed below that bound.
Execution checks this before signing and again against fresh exposure before
broadcast; a stale full-close exemption cannot authorize an undersized partial.

Start the signer with that policy, then run `execute --plan PLAN --socket SOCKET
--state OPERATIONS_DIRECTORY --acknowledge-testnet`. The account lock and pending
reservation span plans, so changing session IDs cannot retry uncertain writes.
It checks venue agent authorization, exact signature recovery, unchanged size,
metadata and price bounds. A full close is followed by cancellation of only
explicitly owned exits, after fresh flatness checks. A partial close retains exits.

`reconcile --plan PLAN` only reads venue outcomes. Add `--state OPERATIONS_DIRECTORY`
to record the observation and release a matching pending reservation when fresh
terminal evidence proves the close resolved. Missing order IDs remain uncertain.
No automatic retry occurs; do not remove state to bypass a reservation. Use the
same operations directory for every workflow controlling this account.

Eight additional deterministic tests cover adoption, partial/full closes, cleanup,
uncertainty, replay refusal, stale empty DEXs and concurrent/manual position
changes. The real account and real signer have not been used in these tests.

## Step 3: entries, partial-fill repair and exit amendment

Build the unsigned Rust integration binaries once:

```sh
cargo build --locked -p hypercarry-cli -p hypercarry-testnet-operator \
  --bin hypercarry --bin hypercarry-bracket-journal
```

`plan-open --input PRIVATE_INPUT --agent AGENT --key-id ALIAS --analytics-bin
ABSOLUTE_HYPERCARRY --journal-bin ABSOLUTE_BRACKET_JOURNAL --output PLAN` consumes
the existing `PlanInput` format (`docs/trade-plan.example.json`), with explicit
testnet network, public account, market and desired risk bounds. It gathers
native/builder metadata, maintenance tiers, Unified available USDC and the
account fee rate, then invokes the existing checked-Decimal Rust planner through
`trade-calculate`. The calculator is offline, accepts bounded JSON on stdin,
and always returns `executable: false`; supplied data is not live evidence.
Scoped builder market names are validated without admitting paths or empty scopes.

Execution uses the same `execute` command and signer approval as closing. Both
Rust executable hashes are bound into the plan. The initial profile supports
Unified USDC accounts and isolated 1x only. The fee calculation conservatively
omits referral/aligned-quote discounts while applying reported builder fee scale
and growth mode; it is a ceiling, not an exact fee prediction. It refuses unknown
fee fields rather than guessing. The live book, available reserve, fees and
actual leverage setting are rechecked before entry.

The `hypercarry-bracket-journal` bridge uses the existing `BracketManager` and
`FileJournal`, rather than a separate lifecycle implementation. Entry reservation
is durable before signing; the bridge itself cannot sign or submit. Terminal
partial fills get one bounded missing-exit repair at actual size. A new explicit
`ProtectionRepairReserved` event records real replacement client IDs without
rewriting the immutable specification or fabricating old IDs. Unknown entry
outcomes cannot initiate repair. Current recovery adapters that expect the original
exit IDs will conservatively refuse replacements; do not silently hand such a
journal to the old recovery host.

Successful entries save `STATE/SESSION/position.json` for subsequent close/protect
plans. `reconcile --plan PLAN --state STATE` replays the Rust lifecycle using fresh
venue evidence. Configuration-only failures can be released after signed expiry
and verified flatness, without submitting an entry. Entry uncertainty is retained.

`plan-protect --adoption POSITION_JSON --agent AGENT --key-id ALIAS --take-profit
PRICE --stop-loss PRICE --output PLAN` prepares a separate exit amendment approval.
The current profile permits tightening an existing stop, and checks its execution
bound against the observed liquidation price. New full-size exits must be observed
before old approved IDs are retired. Rejected or uncertain replacements leave old
protection in place. On success, use the new attempt's `position.json` thereafter.

Seven further Python integration tests exercise the real Rust calculator/journal,
full/partial fills, unknown entry outcomes, reserve refusal and replacement-before-
cancel ordering against a scripted venue. Rust tests cover scoped names and durable
replacement IDs; strict Clippy passes for the affected crates. These are offline
integration results, not credentialed deployment evidence.

## Step 4: persistent ledger, cached status and monitoring

`ledger-sync --network NETWORK --account ADDRESS --directory PRIVATE_LEDGER
--start-ms UNIX_MS` ingests account fills, funding and non-funding ledger events.
The optional `--end-ms` defaults to now. Later calls may omit `--start-ms` to
resume with a one-minute overlap. Each response is archived privately; conflicting
duplicates, saturated timestamp boundaries and failed reads do not advance the
ledger checkpoint. The current private JSON store is bounded to 16 MiB; large
histories need a storage migration, not deletion of old evidence.

`ledger-report --directory PRIVATE_LEDGER --start-ms START --end-ms END` separates
observed realized price P&L, funding, fees by token and known contributed capital.
It identifies unclassified transfers, verifies USDC collateral before aggregation,
and excludes spot/non-USDC legs from the known USDC net. It reports requested-window
pagination coverage separately from historical completeness. The API only exposes
its recent fill retention window, so an empty/short response is never claimed as
proof of complete lifetime history. Zero-fee USDC `send` events between native/spot
cash endpoints are classified by account sender/destination: incoming capital,
outgoing capital, or zero for a same-account movement. Other tokens/routes,
nonzero fees and ambiguous identities remain unclassified.

Funding records with non-null `delta.nSamples` are compacted observations. Their
timestamps may label a day instead of an individual settlement; mixing them with
archived hourly rows can double-count. Reports expose `aggregated_funding_coins`
and `funding_aggregation_unresolved: true`, with null `known_net_usdc` for affected
markets. This conservative gap applies even when the aggregate label falls outside
the requested report interval. Raw rows remain preserved for independent
reconstruction; automatic aggregate expansion or deduplication is not implemented.
The known-net total excludes those unqualified markets. Equity reconciliation
returns `incomplete` rather than claiming a matched window. Complete historical
and automated spot-hedge accounting remain unfinished.

`watch --network NETWORK --account ADDRESS --analytics-bin ABSOLUTE_HYPERCARRY
--directory PRIVATE_ARCHIVE --duration-seconds 3600` runs the existing Rust
`portfolio --require-exits --archive-dir` collector every 30 seconds by default.
`--once` performs one observation. `--websocket` adds order/fill wake-ups with
reconnect reconciliation, throttled to at most two full REST scans per minute.
Optional `--ledger-dir PRIVATE_LEDGER --ledger-start-ms START` maintains the ledger
alongside snapshots; subsequent runs resume its checkpoint. Ledger failure has its
own `ledger-health.json` and does not turn an account observation into a success.
Review aggregate rate usage with other processes before installing continuously.

The existing archive creates `notifications.jsonl` for the existing notification
outbox; this command does not configure a provider or send external messages.
Use the existing `portfolio-heartbeat-check` independently to detect a stopped
collector. No background service is installed automatically.

`status --network NETWORK --account ADDRESS --cache-dir PRIVATE_ARCHIVE` reads
`latest.json` immediately, includes age and usability, and returns nonzero for
missing/stale evidence. A failed collection replaces cached success with unknown;
execution always queries fresh venue data and never trusts this display cache.

Eight additional offline tests cover overlapping pagination, conflicting records,
rollback on failed ingestion, capital/P&L separation, unsupported currency and
ledger types, collector identity, cache invalidation and the Rust archive path.

## Step 5: integration verification and release boundary

Use `tools/check_local_operator.py` under the pinned SDK environment for the full
offline suite with no skipped tests and optional private artifact receipts.
The same check runs in CI. Final submission checks run after signing, including
fresh exposure/book state and agent authorization; entries also recheck reserve,
fees and venue capacity. The suite tests real socket signatures with public
fixture keys and refuses to broadcast on signature mismatch, post-sign resize or
agent revocation. Session policies bind the full plan, including its risk inputs.

See [the validation and operator handoff](local-operator-validation.md) for the
credentialed testnet scenarios, independent review and remaining mainnet release
work. Those external requirements remain pending. This implementation does not
change the mainnet release gate or authorize the deferred SP500 trade.

## Large discovery sets

The local Python reader uses fresh WebSocket bootstrap when more than 20 DEXs
are discovered (maximum 512). It requires a complete `allDexsClearinghouseState`
snapshot and an initial full `openOrders` snapshot for every discovered DEX,
then explicitly re-subscribes for a second complete position snapshot. Financial
position identities must match. Active DEXs and native are independently checked
through REST; changed positions/order IDs refuse the observation. Final signed
actions still require target REST/book/agent checks. This is a fresh observation
path, not a persistent stream cache or unattended recovery service. Each bootstrap
uses a new bounded connection; respect the venue's 30-new-connections/minute limit.
The Rust recovery transport also implements complete fresh stream bootstrap
and target REST consistency checks. The standalone Rust portfolio observer used
by `watch` now supports up to 512 DEXs with fresh stream scope above 20, native/active
REST checks, unchanged discovery and bounded freshness. An unsigned testnet check
covered 288 DEXs; continuous monitoring deployment and human alert acknowledgement
remain pending. Mainnet remains read-only in this package.

Protocol references: [subscriptions](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions)
and [rate limits](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits).

## Recovery handoff from an owned local entry

`plan-recovery --entry-plan OPEN_PLAN --state OPERATIONS --limits LIMITS_JSON
--recovery-bin /absolute/path/hypercarry-testnet-recovery --signer-socket SOCKET
--output HANDOFF_DIRECTORY` replays the original bracket and derives its effective
exit IDs without changing the approved specification. Resolve any pending local
attempt first. The output contains `handoff.json`, `policy.json` and
`recovery-config.json`; review the complete handoff and binary digest before use.
Start the existing `signer` command with this policy, then run the pinned recovery
binary with `--config HANDOFF_DIRECTORY/recovery-config.json --acknowledgement
'I ACKNOWLEDGE HYPERCARRY TESTNET EXECUTION AND EXTERNAL EFFECTS'`. The recovery signer grants no entry,
leverage changes or protection amendments, only the exact three owned client IDs
and bounded reduce-only closes. Cumulative signed close size cannot exceed the
observed residual; a rejected/partial attempt may exhaust this conservative
signing budget before the engine's retry count. Renew authority only through a
freshly reviewed residual plan. Recovery freezes entries persistently. Human
review is needed before clearing that freeze; never delete an unresolved gate to
retry. The local host must remain running; this is not host-independent recovery.

## Windowed equity reconciliation

Use `equity-checkpoint --network mainnet|testnet --account ADDRESS --output FILE`
before and after an observation interval. Sync the ledger across both complete
checkpoint windows, then run `equity-reconcile --directory LEDGER --before FILE
--after FILE --output REPORT [--tolerance-usdc 0.01]`. These are unsigned reads.
The current model is specifically Unified USDC: shared spot USDC total is the
marked collateral component, and the expected change is external capital plus
realized price P&L minus fees plus funding plus the change in unrealized P&L.
Never add unrealized P&L to that shared balance a second time or sum per-DEX
account values. Model assumptions and actual residuals need independent review.

The flow interval is `(before.completed, after.completed]`. Valuation reads are
sequential; price movement during either window can leave a residual. Events
inside checkpoint windows, unclassified transfers, non-USDC currencies and
incomplete pagination prevent a matched-window claim. Residuals are reported,
not adjusted away. Vaults, staking, historical gaps and spot-hedge cost basis
remain outside this component model. A matched window is not lifetime returns
or permission to trade. See the official [account modes](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/account-abstraction-modes)
and [P&L definitions](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/entry-price-and-pnl).
