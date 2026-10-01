# CLI contracts v1

The `hypercarry` CLI separates copy-friendly human displays from stable JSON
automation output. Commands that accept `--output json` emit one JSON document
on stdout with `schema_version: 1`; diagnostics and progress remain on stderr.
Decimal values are serialized as base-10 strings so consumers never lose
precision through binary floating point. Tracing, including `--tracing diagnostic`,
also writes to stderr.

Snapshot settlements are validated against the requested coin and inclusive
four-hour window, sorted by timestamp, and deduplicated. Wrong-coin rows,
out-of-window rows, and conflicting duplicate settlements fail as schema errors
(exit code 12). Transient history request failures use the bounded retry policy
shared with backfill.

## Operational commands

| Command | Purpose | External I/O |
|---|---|---|
| `snapshot` | Current context, venue prediction, and recent settlements | Read-only HTTPS |
| `backfill` | Resumable settled-funding history | Read-only HTTPS and local Parquet writes |
| `record` | Public context and L2 capture | Read-only WebSocket and local JSONL/Parquet writes |
| `apr` | Latest realized funding and simple APR | Local Parquet reads |
| `pnl` | Realized carry result for one recorded position | Local Parquet and trade-document reads |
| `basis` | Exact perp/spot basis from explicit prices | None |
| `spread` | Exact interval-normalized funding spread | None |
| `predict` | Causal next-hour estimate from a raw capture | Local JSONL and optional Parquet reads |
| `tui` | Live view over an actively appended raw capture | Local JSONL reads only |

`completions <bash|elvish|fish|powershell|zsh>` and `manpage` generate artifacts
on stdout. For example:

```sh
hypercarry completions zsh > _hypercarry
hypercarry manpage > hypercarry.1
```

## Exact metric contracts

`basis --output json` fields are `schema_version`, `coin`, `quote_unit`,
`perp_mark`, `spot_mid`, `absolute_quote`, `ratio`, and `ratio_percent`.
Prices and absolute basis are USDC; `ratio` is dimensionless and
`ratio_percent` is already multiplied by 100. A zero or negative spot mid is a
configuration error.

```sh
hypercarry basis --coin BTC --perp-mark 101 --spot-mid 100 --output json
```

`spread --output json` fields are `schema_version`, `coin`, `venue_a`,
`venue_b`, `rate_a`, `interval_a_hours`, `rate_b`, `interval_b_hours`,
`hourly_spread_a_minus_b`, and `hourly_spread_bps`. Input rates apply to their
declared settlement intervals. The result always means venue A minus venue B
per hour; both interval values must be positive.

```sh
hypercarry spread --coin BTC \
  --venue-a Hyperliquid --rate-a 0.0001 --interval-a-hours 1 \
  --venue-b Venue8h --rate-b 0.0004 --interval-b-hours 8 \
  --output json
```

`pnl --output json` fields are `schema_version`, `network`, `venue`, `coin`,
`quote_unit`, `side`, `size`, `hedged`, `closed`, `settlement_valuation`,
`valuation_price`, `entry_time_ms`, `exit_time_ms`, `first_settlement_ms`,
`last_settlement_ms`, `settlements`, `dataset_first_settlement_ms`,
`dataset_last_settlement_ms`, `window_fully_covered`, `entry_notional`,
`funding`, `perp_price_pnl`, `spot_price_pnl`, `fees`, `net`,
`return_on_notional`, and `annualized_return`. Monetary values are USDC. The
components sum exactly to `net`; `fees` is reported positive and subtracted.

The position is read from a schema-v1 trade document named by `--trade`, which
is an operator record rather than pipeline output. Unknown fields are rejected.

```sh
hypercarry pnl --network mainnet --coin BTC --dataset data \
  --trade ./trades/btc-carry.json --output json
```

Funding applies to settlements strictly after `entry_time_ms` and at or before
the exit, because a position opened exactly at a settlement did not hold through
it. Settlement timestamps carry millisecond jitter, so a round-hour window can
exclude the settlement it looks like it should contain; the human view prints
the applied range for exactly this reason. `window_fully_covered` is true only
when the dataset holds an observation at or before entry, one at or after the
end, and an unbroken hourly sequence between them. Spanning the endpoints is not
sufficient: a settlement missing *inside* the window understates funding exactly
as a missing endpoint does, and a span check cannot see it. Gaps outside the
window belong to other windows and do not affect this one.

Requiring an observation past the end is stricter than the hourly schedule
demands, in the conservative direction. A trade that has just closed reads as
partial until the next settlement is recorded, which also distinguishes "no
settlement was due" from "recording stopped". An open position is never fully
covered, because funding it will earn has not settled yet — so read
`window_fully_covered` on a **closed** trade document.

The settled-funding dataset stores a rate and a premium, not a mark price, so
valuing each settlement requires an explicit choice. `settlement_valuation`
defaults to `perp-entry-price` (constant notional) and may be `fixed` with an
explicit price. The choice is echoed into the output rather than assumed.
Human percentages are rounded to six decimal places for display; the JSON
contract keeps the exact value.

`apr --output json` reports `contiguous_history_hours` alongside
`available_observations`: the unbroken hourly run ending at the newest
observation, which is what a monitor must assert to trust recent history. A
total count cannot distinguish a complete week from a month with holes in it.
The run is measured backwards from the newest record, so a gap in old history
does not erase it, and it is counted in hours so a continuously recording host
sees it rise every hour rather than only at day boundaries. The human view
states both the total and the run.

The snapshot, backfill, APR, recorder, and predictor schema-v1 contracts are
frozen in their focused documents:

- [settled funding Parquet](settled-funding-parquet-v1.md)
- [live recording](live-market-recording-v1.md)
- [funding prediction](funding-prediction-v1.md)

## Time, rates, and display rules

- Human timestamps are RFC 3339 UTC and carry a `UTC` label. Raw Unix values
  are milliseconds since the epoch.
- Funding rates are decimal ratios unless a `%` or `bps` suffix is present.
- APR is simple annualization, not compounding: hourly rate × 8,760.
- Human decimal output is normalized but never converted through `f64`.
- TUI sign labels (`POSITIVE`, `NEGATIVE`, `ZERO`, `ERROR`) remain visible when
  color is disabled. `--color auto` also honors `NO_COLOR`.

## Configuration and failures

Layered commands resolve command-line flags, then `HYPERCARRY_*` environment
variables, then the named JSON configuration section. Network and coin never
silently default. `HYPERCARRY_REFRESH_MS` and `HYPERCARRY_COLOR` configure the
TUI; refresh intervals are bounded to 100–60,000 ms.

Stable exit codes are: 0 success, 1 internal, 2 usage, 3 reserved
unimplemented, 10 configuration, 11 network, 12 schema, 13 storage, 14 partial
data, 15 output, and 130 cancellation.
