# CLI contracts v1

The `hypercarry` CLI separates copy-friendly human displays from stable JSON
automation output. Commands that accept `--output json` emit one JSON document
on stdout with `schema_version: 1`; diagnostics and progress remain on stderr.
Decimal values are serialized as base-10 strings so consumers never lose
precision through binary floating point.

## Operational commands

| Command | Purpose | External I/O |
|---|---|---|
| `snapshot` | Current context, venue prediction, and recent settlements | Read-only HTTPS |
| `backfill` | Resumable settled-funding history | Read-only HTTPS and local Parquet writes |
| `record` | Public context and L2 capture | Read-only WebSocket and local JSONL/Parquet writes |
| `apr` | Latest realized funding and simple APR | Local Parquet reads |
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
