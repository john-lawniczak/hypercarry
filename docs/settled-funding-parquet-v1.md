# Settled-funding Parquet schema v1

This document freezes the first durable dataset contract. The executable source
of truth is `hypercarry_storage::settled_funding`; its schema text is parsed by
the official Rust Parquet implementation during normal tests. The production
dataset implementation converts validated records to Arrow, writes Parquet,
reads it back through the same contract, and compares every column in tests.

## Identity and ordering

Each observation is uniquely identified by:

```text
(network, venue, coin, settlement_time_ms)
```

Writers must sort lexicographically by that key and reject or deterministically
deduplicate repeated keys before committing a file. Mainnet and testnet are
different identities even when every other field matches.

## Columns

| Column | Parquet type | Meaning |
|---|---|---|
| `schema_version` | required `INT32` | Always `1` for this contract. |
| `software_version` | required UTF-8 | Hypercarry version that normalized the row. |
| `network` | required UTF-8 | `mainnet` or `testnet`. |
| `venue` | required UTF-8 | Source venue; `hyperliquid` for `fundingHistory`. |
| `coin` | required UTF-8 | Exact source market symbol. |
| `settlement_time_ms` | required UTC timestamp (ms) | Funding settlement instant. |
| `funding_rate` | required decimal `(38,18)` | Settled rate, represented exactly. |
| `premium` | required decimal `(38,18)` | Source premium, represented exactly. |
| `source_endpoint_class` | required UTF-8 | `official` or `development`. |
| `ingestion_time_ms` | required UTC timestamp (ms) | Time the response was normalized. |
| `request_start_time_ms` | required UTC timestamp (ms) | Inclusive source request lower bound. |
| `request_end_time_ms` | required UTC timestamp (ms) | Inclusive source request upper bound. |

Values with more than 18 decimal places are rejected instead of rounded. A new
schema version is required to change a column name, meaning, logical type,
nullability, decimal scale, identity key, or partition interpretation.

## Partition layout

The Hive-style directory is:

```text
settled_funding/
  schema_version=1/
  network=<network>/
  venue=<percent-encoded venue>/
  coin=<percent-encoded coin>/
  settlement_date_utc=<YYYY-MM-DD>/
  data.parquet
```

Partition values are percent-encoded UTF-8, preventing a source symbol from
creating an unintended path. Date partitioning uses the settlement instant in
UTC, not ingestion time. Each daily partition has one canonical `data.parquet`
file, replaced through a same-directory temporary file and atomic rename.

## Commit and resume behavior

- Existing and incoming rows are merged in identity order. Identical market
  values collapse; different funding-rate or premium values for one identity
  are rejected as a conflict.
- When duplicate provenance differs, the canonical row is selected by earliest
  ingestion time, then official before development endpoint, software version,
  and request bounds. The result does not depend on input order.
- Data files are flushed and synchronized before atomic replacement. The parent
  directory is synchronized on Unix so a successful commit survives a crash.
- `_checkpoint.json` is stored per network/venue/coin stream and advances only
  after all affected data partitions are durable. It never moves backward.
- Resume timestamps are inclusive. A crash after a data replacement but before
  its checkpoint update therefore causes a safe replay, which deterministic
  deduplication collapses.

## Provenance invariants

- Request bounds are inclusive and `start <= end`.
- Each observation's settlement time must fall inside its recorded request
  window.
- Network is stored in both the identity and partition path.
- Endpoint class records whether an official or explicit development endpoint
  supplied the response; raw endpoint URLs are not persisted.
- Schema and software versions are stored on every row so copied files remain
  self-describing outside their original directory.
