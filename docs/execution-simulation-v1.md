# Execution simulation and dry-run v1

M6 introduces `hypercarry-execution` as an optional venue-neutral library. It
has no dependency on the CLI, analytics core, recorder, or storage crates, and
the default CLI has no dependency on it. The crate contains no exchange SDK,
HTTP/WebSocket client, wallet, key storage, or signer implementation.

## Shared validation path

Strategies may construct only an inert `OrderIntent`. Before any adapter sees
an order, the dry-run pipeline:

1. resolves `MarketMetadata` for the exact venue and market;
2. validates schema, correlation ID, names, positive price/size, and timestamp;
3. rounds size down to the venue step and rejects a result below minimum size;
4. rounds buy limits down and sell limits up to avoid making an intent more
   aggressive;
5. obtains a deterministic `RiskDecision` and fails closed if policy evaluation
   errors;
6. journals the decision and exact `ValidatedOrder` before invoking an adapter.

`ExecutionAdapter`, `RiskPolicy`, `MarketMetadataResolver`, and `Signer` are
traits. M6 deliberately supplies no `Signer` implementation, and neither the
simulator nor dry-run type accepts one.

## Simulator

`SimulatorAdapter` consumes caller-provided, timestamp-ordered top-of-book
frames. Configuration explicitly fixes latency, fee basis points, adverse
slippage basis points, cancellation time, and whether fill or cancellation wins
an equal-timestamp race.

- Frames before `created_at_ms + latency_ms` cannot fill.
- Buys execute against available ask size; sells execute against bid size.
- The adverse-slippage price must still cross the validated limit.
- Each frame's available size can cause a partial fill; later frames may fill
  the remainder.
- Quote fee is `quantity × price × fee_bps / 10,000` using exact decimals.
- Outcomes distinguish filled, partially filled then cancelled, cancelled,
  rejected, and no fill.

Available size is a deterministic per-frame assumption, not a queue-position
or market-impact model. Results are research fixtures, not execution forecasts.

## Non-signing dry-run

`DryRun` combines a metadata resolver, risk policy, durable journal, and
`DryRunAdapter`. An allowed action returns `dry_run_recorded`; it never means a
venue accepted an order. A policy rejection returns a structured rejection and
never writes an exact-action event. There is no submission or network method in
the adapter.

## Journal schema v1

The append-only JSONL journal assigns contiguous `sequence` values beginning at
zero. Every record contains:

- `schema_version` (`1`), `sequence`, `recorded_at_ms`, and `correlation_id`;
- one tagged event: `risk_allowed`, `risk_rejected`, `exact_action`, or
  `state_observed`.

Opening an existing file validates every schema version and sequence before
allowing an append. Each new event is serialized fully before write, flushed,
and synchronized. The schema contains no signature, private key, credential,
request header, or arbitrary signing payload. Correlation IDs are restricted to
bounded safe ASCII characters.

M7 extends the file journal with process-independent writer locking and strict
lifecycle recovery while preserving these v1 records. Its safety contracts are
documented in `execution-safety-v1.md`. M8 remains the earliest milestone
permitted to add testnet submission.
