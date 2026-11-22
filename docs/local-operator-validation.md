# Local operator verification and remaining release work

Local testnet signing and a separately gated, default-off mainnet adapter are
implemented. Offline success does not authorize mainnet trading, prove venue
response contracts or replace independent custody/signing review.

## Reproducible offline verification

With the pinned SDK cached, run from the repository root:

```sh
uv run --offline --no-project --with hyperliquid-python-sdk==0.24.0 \
  python tools/check_local_operator.py
```

The checker builds the Rust integration binaries, checks the SDK version and
requires the entire local suite to pass with no skipped tests. Optional
`--report /absolute/private-directory/offline.json` writes a private receipt
binding source, dirty-tree status, executable hashes, SDK version, timestamp and
test count. Credentialed verification, independent review and mainnet authority
remain false. A dirty-tree receipt cannot freeze a release candidate.

The local suite covers policy/nonce budgets, SDK/Rust signature compatibility,
real Unix sockets, lost responses, post-signing state changes, agent revocation,
planning/journaling, partial fills, protection repair, recovery handoff, complete
DEX bootstrap, capacity identity, ledger coverage, equity residuals and monitoring.
Only deliberately public unfunded fixture keys are used. Repository checks also
include formatting, strict all-feature Clippy, Rust 1.94.1, release builds,
dependency/advisory policy, tooling tests and the filtered-export checker.

## Minimum-close regression contract

Partial closes require rounded quantity × approved limit ≥ 10 USDC. Planning,
execution preflight and the final post-signing check enforce this against fresh
residual exposure. A full residual close remains exempt. For a **synthetic**
0.0008 BTC position at a 25,000 limit, closing 0.0002 is a $5 partial and is
rejected; closing 0.0005 is a $12.50 partial and meets the minimum. Closing an
entire 0.0001 residual is allowed through this local check despite its $2.50
notional. These invented examples are not venue executions or guarantees.

Tests cover both directions, exactly 10, rounding below the boundary, old sealed
plans, and fresh exposure growth/shrinkage. Real socket tests verify that growth
after signing revokes the full-close exemption before broadcast. Existing
protection is retained. An explicit venue rejection must be reconciled; a zero
process exit code alone does not establish a fill.

## Qualification records

Use [the attended runbook](attended-testnet-session.md) to freeze clean source
and prepare locally approved testnet sessions. Retain real identities, fills,
accounting, manifest hashes and human attestations privately. Historical runs
on other source or binary identities cannot qualify a changed candidate.
A partial close is not a partial entry fill. No public example or offline test
count completes the credentialed scenarios, human review or release decision.

## Credentialed operator exercises still required

1. Provision a dedicated **testnet agent** and funded testnet trading account.
   Verify the agent's public binding and expiry at the venue. Keep the encrypted
   keystore, password, private state and evidence outside version control. The
   account and agent must differ; the signer refuses the owner key.
2. Run the commands in [the integration guide](local-operator-integration.md)
   with minimum practical testnet size and an explicit reserve. Start with one
   supported native market, then a supported builder market with USDC collateral.
   Record the actual SDK/API response formats and available depth. A scripted
   venue's behavior is not evidence that the venue behaves identically.
3. Retain at least three distinct credentialed sessions, with independent human
   review, covering the scenarios below. Bind each session to exact clean source,
   lockfile, binaries, policy digest, public identities and observation window.
   Do not save raw signatures or passwords. Independently re-query final venue
   positions and orders rather than treating the submission response as final.

| Scenario | Required evidence |
| --- | --- |
| Full entry and close | Actual fill, both live exits, final flatness and owned-exit cleanup |
| Partial entry | Actual residual size, real replacement IDs, full observed protection |
| Rejected replacement exit | Old protection retained; incident visible; no false Open claim |
| Lost acknowledgement | Original IDs/reservations retained; no second entry; authoritative reconciliation |
| Restart | Same journal, nonce domain and account gate; no replay of uncertain work |
| Manual resize or native-stop race | Fresh residual checks; reduce-only behavior; no reversed exposure |
| Revoked/expired agent | No broadcast after failed authorization; no automatic restoration |
| Stale REST or disconnected stream | Unknown/stale state; resynchronization; no cache-based submission |
| Signer/host loss | Native exits remain; independent liveness detection and documented manual recovery |
| Alert route | Real operator-visible delivery plus evidence of failed delivery detection |

4. Obtain an independent review of the persistent Python custody process,
   SDK serialization, signer policy, plan digests, nonce storage, final submission
   checks, durable reservations and Rust bridge. Python/SDK key copies cannot be
   guaranteed erased. The current OS-user boundary does not protect against
   hostile software running under that same identity.
5. Validate the legacy Rust recovery handoff: recovery now pins replayed repair
   IDs separately from the original specification and checks builder asset mappings.
   It freezes local entries and refuses a pending Python execution reservation.
   Resolve that reservation through its owning workflow before handoff. An old
   recovery journal whose pinned policy differs is refused, never rewritten.
   The `plan-recovery` bundle supplies a compatible exact-client-ID signer policy;
   large DEX sets use fresh complete subscriptions and target REST checks. These
   transport reads passed on testnet; credentialed recovery and independent
   review remain required. The new explicit close command is not an unattended recovery
   service. Install independent monitoring/recovery appropriate to the chosen
   operating model before claiming availability during local host loss.
6. Reconcile contributed capital, transfers, fees, funding and both carry legs
   against account equity over a fixed observation window. Current ledger reports
   remain observed cash-flow reports, with retention and valuation gaps disclosed.
   Automated spot-hedge management remains a later carry-product requirement.
7. Review and freeze a **local bracket mainnet release profile**. The existing
   gate covers one GTC canary and cannot be widened by changing a network string.
   Fulfill its independent evidence requirements and bind a new human release
   decision before enabling the implemented, default-off mainnet submission path.

## Operational limits to preserve

- All processes controlling an account must share the operations directory and
  use a dedicated agent nonce domain. Another app/device is outside that lock.
- An uncertain attempt keeps the account reservation. Use `reconcile`; inspect
  the venue manually when evidence remains unknown. Never delete state to retry.
- Reserve checks run before entry and are checked after fills. A favorable gap
  on a short IOC can still increase actual collateral usage; a reserve breach
  retains the account gate and requires intervention. No execution guarantee is
  claimed across the network interval after the last check.
- Close/protection checks are repeated after signing, before broadcast. Existing
  exits are retained while exposed. Replacement cancellation re-verifies the new
  pair after signing as well. Every request has a short expiry and no implicit
  HTTP write retries.
- The observer creates existing notification-source events, not delivery proof.
  No daemon, scheduler, external message route or real account change is installed
  by building/testing this integration.
