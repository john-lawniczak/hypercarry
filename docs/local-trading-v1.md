# Local operator trading: implementation and release boundary

Status: 2026-11-16. Account inspection and previews are usable. Bracket wire
encoding and a journal-backed lifecycle engine are implemented and tested
offline. The [experimental local operator integration](local-operator-integration.md)
now connects the Rust planner/journal, a persistent agent signer and a testnet
entry/close/protection driver. **General mainnet bracket trading is not released.**
The existing mainnet canary gate stays closed. This document does not authorize
real-money order submission.

## What runs now

The normal CLI remains unsigned and has no dependency on a key loader or the
execution crates. Use the trading account address, not the API agent address:

```sh
ACCOUNT='0x1111111111111111111111111111111111111111' # replace with public account
cargo run -p hypercarry-cli --locked -- account --network mainnet --address "$ACCOUNT" status
cargo run -p hypercarry-cli --locked -- account --network mainnet --address "$ACCOUNT" --output json positions
cargo run -p hypercarry-cli --locked -- account --network mainnet --address "$ACCOUNT" orders
cargo run -p hypercarry-cli --locked -- account --network mainnet --address "$ACCOUNT" fills
cargo run -p hypercarry-cli --locked -- account --network mainnet --address "$ACCOUNT" funding --start-ms 1794268800000 --end-ms 1794355200000
```

`status` and `positions` currently return the same combined snapshot: positions,
spot balances, available Unified USDC, open orders and derived isolated health.
They support human and JSON output. Orders, fills and funding return JSON in
either output mode. These commands require an explicit network/address and
reject the analytics `--config` option rather than silently ignoring it.

Under the hood these are HTTPS POST requests to the official `/info` endpoint:
`clearinghouseState`, `spotClearinghouseState`, `frontendOpenOrders`, `meta`,
`userFills` and `userFunding`. POST does not necessarily mean a mutation; these
request types only read public account state. No signature, wallet unlock, RPC
gas or API key is needed.

The snapshot records its start/end times. Sequential reads are not atomic and
may span a changing position. Scope is **native perps and shared spot**, not an
all-DEX risk report. Cross-position health is not inferred. Available funds use
the venue's `tokenToAvailableAfterMaintenance` USDC entry, never the total spot
balance as a substitute. Missing availability stays unknown.

Funding pagination overlaps the inclusive boundary, deduplicates by time/hash/
coin, and rejects conflicts or stalled pagination. Recent fills are explicitly
bounded to the venue's most recent 2,000 records, not a complete ledger. Fills
can include native, builder and spot markets; their envelope now says so rather
than labeling everything native-only. Funding is labeled by its requested
window; native order queries can also include spot orders.

## All-DEX portfolio and exit coverage

The native `status`, `positions` and `orders` commands retain their original
scope. Use the new `portfolio` command when an account can hold HIP-3 positions:

```sh
cargo run -p hypercarry-cli --locked -- account --network mainnet --address "$ACCOUNT" portfolio --require-exits
cargo run -p hypercarry-cli --locked -- account --network mainnet --address "$ACCOUNT" --output json portfolio
```

It discovers `perpDexs` and reads shared spot balances once. Up to 20 DEXs use
`clearinghouseState` and `frontendOpenOrders` REST reads for every DEX. Larger
sets (up to 512) use fresh `allDexsClearinghouseState` plus one `openOrders`
subscription per DEX, with a second acknowledged position subscription after all
initial order snapshots. Native and every DEX with positions or orders then also
pass paired REST reads and stream/REST position and order-ID agreement. This keeps
flat builder scope complete without issuing hundreds of REST requests. Stream
bootstrap has a 15-second deadline and bounded frames/messages; incomplete scope
fails. See [stream subscriptions](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions). Position reads bracket
each order read: quantity, entry price and margin configuration must stay equal;
ordinary mark/P&L changes are allowed. Native requests omit
`dex`; builder requests use the exact discovered name. Discovery is checked
again at the end. See the official [account/order info API](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint)
and [perpetual info API](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint/perpetuals).

Portfolio JSON schema **2** preserves both position reads, venue timestamps,
observation windows, order/client IDs, order timestamps, trigger conditions,
pending children, reported size and execution bounds, plus matching exit IDs
and orphan-trigger IDs. Pending children never count as independent exits.
Native-only snapshot schemas remain unchanged. It does **not** sum shared spot equity with isolated margin,
convert other collateral currencies to USD, infer withdrawal limits, include
vaults/subaccounts, or reconstruct historical P&L. Missing Unified availability
remains unknown. Discovery covers at most 512 DEXs; malformed or unavailable
responses fail rather than silently omitting that DEX. A changed discovery set,
a monotonic two-minute deadline, or any position response timestamp older than
60 seconds at completion (or over five seconds in the future) also fails.
This includes flat accounts: stale emptiness is not evidence of no exposure.

Coverage means at least one independently full-size reduce-only TP and one SL
with the closing side, recognized order type, positive prices, trigger on the
expected side of the observed mark and a bound marketable at that trigger.
Split-size exits are conservatively not summed. Unknown types, zero-size dynamic
position exits and already-crossed triggers do not count. Limit exits can count
as observed coverage; inspect the reported type and bound because neither limit
nor market triggers guarantee fills. A trigger without a corresponding open
position is reported as an orphan, without canceling it.

`--require-exits` prints the completed report then returns **exit code 14** if
coverage is incomplete or orphan triggers exist. Read/schema/freshness failures
also return nonzero, without a completed report. A flat account with no orphan
triggers passes. This is a point-in-time observation, **not** an execution gate,
liquidation guarantee, atomic account snapshot or installed monitoring service.
Bracketing detects observed changes but cannot eliminate all races: positions
can change and return to the same state between reads, and orders can change
immediately after observation. Coverage is never an execution authorization.

To archive every read attempt privately (Unix):

```sh
mkdir -p data/account-snapshots
chmod 700 data/account-snapshots
cargo run -p hypercarry-cli --locked -- account --network mainnet \
  --address "$ACCOUNT" --output json portfolio --require-exits \
  --archive-dir data/account-snapshots
```

The directory must already exist, be owned by the current user and have no
group/other access. The writer pins its directory descriptor, refuses a final
symlink, creates mode-0600 files, syncs bytes and atomically publishes a unique
`Z-observation-*.json` without overwriting history. A SHA-256 receipt goes to
stderr. Storage failures return code 13; no successful stdout report is emitted.
A completed snapshot with missing exits is archived **before** exit code 14.
Read failures create an observation with `snapshot: null`, unknown coverage and
an error, then return nonzero. These must never be counted as flat/protected.
A crash can leave an uncommitted `Z-*.tmp`; only completed `.json` records count.
A sync error after publication can leave a completed record despite an error;
preserve it. Serialization is bounded to 16 MiB, and snapshot network, account,
schema and timestamps must match the enclosing observation. Directory ACLs and
privileged access are outside the Unix mode/owner check. SHA-256 receipts detect corruption, not malicious tampering.

Archives are point-in-time observations, not a paginated fill/funding/transfer
ledger or a running monitor. They do not install alerts, modify trades or load
credentials. See [the privacy boundary](public-privacy-boundary.md) for Git and
export rules. Private archives and wallet state are not backed up by Git.

## Preview a new trade

Copy [trade-plan.example.json](trade-plan.example.json) to a private file under
the ignored `data/` directory. Replace its example account and use current
prices for the desired market. The example is not a trade recommendation.

```sh
cargo run -p hypercarry-cli --locked -- trade-preview --plan data/my-plan.json --output json
```

The command reads live metadata, mark price, free collateral, positions and open
orders. The initial preview scope requires a flat native account with no native
open orders. It refuses to adopt or add to an existing UI position. It does not
verify all-DEX flatness and cannot authorize execution.

The planner uses checked Decimal arithmetic for quantity/price precision,
directional entry/exit bounds, margin, an explicit fee assumption, cash reserve,
and maintenance coverage at both the stop trigger and execution bound. Percent
inputs are percentage points: `"0.5"` means 0.5%, not 5%. The CLI currently
uses a disclosed 0.10% taker-fee assumption; a future submitter must verify the
account's actual fees. Funding and exit fees are excluded from gross P&L.

All previews say `executable: false`. A short IOC sell sets a minimum acceptable
sale price, not a maximum sale price. A favorable upward price gap can therefore
exceed the estimated entry notional/margin budget. The preview's notional budget
must not be advertised as an absolute execution cap. Live policy must constrain
quantity, collateral and actual fills as well as recheck market freshness.

## Maintenance and stop-loss semantics

Hyperliquid liquidates when the relevant equity falls below maintenance. For
each margin tier, maintenance is notional times its maintenance rate minus the
accumulated tier deduction. The rate is `1 / (2 * maximum_leverage_for_tier)`;
the selected trading leverage is not the denominator in that maintenance rate.
See the official [margin tiers](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/margin-tiers)
and [liquidation formula](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/liquidations).

The reader derives an explanatory ratio, not a venue-provided "health factor":

```text
isolated equity = rawUsd + signed_quantity * mark_price
coverage ratio  = isolated equity / maintenance_margin
liquidation boundary: ratio = 1
```

`Position::health_at` evaluates another price while holding collateral,
quantity and funding fixed. It is a scenario, not a future guarantee. The live
API liquidation price remains separately displayed.

A short exits by **buying**. TP/SL mark-price triggers activate an attempt to
close; neither a trigger nor a market label guarantees a complete fill. UI
market TP/SL uses a 10% execution tolerance, separate from the entry's slippage
setting. See [official TP/SL behavior](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/take-profit-and-stop-loss-orders-tp-sl).
Partial parent fills and cancellation can leave the remaining position without
active child exits. Always reconcile the actual position and both exits.

## Implemented library boundaries

| Component | Implemented | Not implied |
|---|---|---|
| `core::account` / `core::portfolio` | Strict account decoding, Unified availability, tiered native maintenance, funding pagination, discovered all-DEX observation and exit coverage | Complete historical ledger, continuous monitor or execution authorization |
| `core::trade_plan` | Rounded isolated previews, reserve and stop-buffer checks | Durable execution approval or liquidation guarantee |
| `signing::bracket` | Leverage/margin actions, IOC/limit/trigger orders, bracket grouping, cancels, network/expiry-bound signing | Permission to submit these through existing canary services |
| `execution::bracket` | Append-before-state-change journal, one entry reservation, restart reconciliation, exit coverage, deduplicated fee/funding cash flows | Mainnet execution authority or automatic adoption of manual positions |

The new codec is separate from `Action`, which remains the narrow canary codec.
Its structural checks do not replace an action policy. A normal bracket requires
one entry, then matching reduce-only TP and SL exits. Transfers, withdrawals,
builder configuration and unknown fields are not accepted. Fourteen vectors
cover both networks and are generated from official Python SDK 0.24.0; the old
28 vectors remain unchanged. Run the independent verifier with:

```sh
uv run --no-project --with hyperliquid-python-sdk==0.24.0 python tools/verify-signing-vectors.py
```

The lifecycle engine only accepts observations after an owned entry reservation.
Unknown acknowledgements require reconciliation; a restart cannot replay a new
entry. Missing exits, unexpected size/direction, unmanaged orders and revoked
authority prevent an `Open` protection claim. A flat position with orphan exits
is not `Closed`. The full-plan digest is part of the immutable specification;
the lifecycle's approval event is not a mainnet capability. Journal files must
be dedicated to one trade. Recorded cash flows become final net P&L only after
flatness and complete fill/funding history have been independently established.

## Stop-failure recovery update

The [bounded recovery implementation](stop-recovery-v1.md) adds a durable driver,
testnet REST/signing adapter and local testnet host. It closes fresh residuals,
preserves native exits until flat, and emits atomic journal alerts to the
[notification worker](notifications-v1.md). This completes the bounded recovery
mechanism and local notification delivery components. The compatible scoped
agent signer and journal-derived handoff are implemented; credentialed recovery
and independent review remain required.

## Remaining local operator release work

The testnet integration supplies planning/execution, scoped signing, adoption
and closes, protection repair/amendment, large-DEX discovery, recovery handoff,
ledger ingestion, Unified-USDC reconciliation and monitor commands. Recovery
retains repaired exit IDs separately from the original specification and checks
builder asset mappings. Completed assembly tasks have been removed from this list.

The deployment target is an operator-run local process. Remaining work:

1. Run three credentialed testnet lifecycle/fault sessions, including actual
   partial fills, rejected protection, lost acknowledgement, restart, manual
   resize, stop/sibling races and agent revocation. Exercise the implemented
   recovery handoff and obtain independent final-state and custody/signing review.
   Start with [the attended runbook](attended-testnet-session.md).
2. Install monitoring and confirm human alert visibility. The Rust observer now
   supports 512 DEXs and has passed a live 288-DEX unsigned testnet check. Console display and a
   stopped-collector heartbeat check have been exercised, but continuous and
   host-independent monitoring/recovery still need deployment and evidence.
3. Retain fresh paired valuation checkpoints over real trade/funding/transfer
   windows and complete historical and spot-hedge accounting. A private historical
   closed-trade window matched after independent hourly funding reconstruction;
   exact baseline timing and automated aggregate reconstruction remain gaps.
4. Qualify [the default-off local mainnet bracket adapter](local-mainnet-bracket-pilot.md).
   Freeze exact artifacts before testnet sessions, retain their reviewed evidence,
   and obtain the required exact-plan human release decision. The single-GTC canary gate cannot
   be reused as bracket authorization.
5. If in-repository funding/agent registration is needed, add it as a separate
   owner-authorized workflow with verified chain, token, recipient and credit.
   The current workflow uses existing funded accounts and approved agents;
   native trade signing does not authorize transfers.

The [validation handoff](local-operator-validation.md) owns the scenario matrix
and evidence requirements. Mainnet account reads and previews remain available
without custody; generic mainnet bracket submission remains disabled.
