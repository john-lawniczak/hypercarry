# Live market recording contracts v1

M3 deliberately stores one session in independent layers. The raw layer is the
replay source of truth; the normalized layer is a rebuildable analytical
projection; diagnostics and derived metric outputs carry their own schema
versions. None of these contracts contain wallet or private-account data.

## Session layout

For network `testnet` and session `session-<unix_ms>-<pid>`:

```text
<dataset>/raw/schema_version=1/network=testnet/<session>.jsonl
<dataset>/normalized/schema_version=1/network=testnet/<session>.parquet
```

Session files use create-new semantics and are never silently overwritten.

## Raw JSON-lines v1

Every received WebSocket text frame is appended before it is offered to the
bounded normalization queue. One line contains:

- `schema_version`, fixed at `1`;
- `session_id`, `network`, `connection_id`, and monotonic receive `sequence`;
- local `received_at_ms` in Unix milliseconds;
- a tagged `kind`: `frame`, `disconnect`, or `stale`;
- exact frame `payload`, disconnect `reason`, or detected `silence_ms`.

Disconnect and stale boundaries make reconnect behavior observable in local
replay. Readers accept additive fields within v1 but reject an unknown schema
version explicitly. Frames discarded from normalization under pressure remain
in this raw log.

## Normalized Parquet v1

Supported `activeAssetCtx` and `l2Book` frames become one row per accepted,
non-duplicate event. Rows retain receive order (`connection_id`, `sequence`),
source time when supplied, an exact event identity, and canonical payload JSON.
Frequently queried values are projected into exact Decimal128(38,18) columns:

- asset context: funding, open interest, oracle, mark, and mid;
- book context: best bid/ask price and size.

The complete depth remains in `payload_json`, allowing a later schema to
project more levels from raw capture without changing v1. Parquet is written in
bounded row groups and finalized on clean cancellation. Exact duplicate frames
are suppressed; distinct payloads at the same source timestamp are preserved.
Out-of-order and stale source timestamps remain stored and are counted in
diagnostics.

## Backpressure and health v1

Raw capture is not queued behind parsing. Normalization uses a bounded Tokio
channel (default 1,024 frames) and the stable
`drop_newest_normalized` policy. A full queue drops only the newest normalized
projection, increments `dropped_normalized_frames`, and retains the frame in raw
capture for later replay.

The final JSON diagnostic contract has `schema_version: 1` and reports network,
session bounds, queue policy/capacity, connections, reconnects, heartbeats, raw
and normalized frame counts, drops, duplicates, out-of-order/stale frames,
parse errors, and both output paths. Tracing emits metadata-only connection,
staleness, drop, and parse health while the command is running.

## Deterministic replay

`ReplayTransport` streams JSONL in constant memory, strictly in file order, and
yields frame, disconnect, and stale events without network access or wall-clock
scheduling.
Parsers and normalized schemas can therefore be tested or rebuilt against the
same immutable receive sequence after code changes.
