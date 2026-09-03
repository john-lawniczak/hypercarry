# Operator recovery

Hypercarry preserves raw inputs and advances durable checkpoints only after
successful writes. Recovery should therefore reuse durable data instead of
manually editing JSONL, Parquet, or checkpoint files.

## Backfill

Ctrl-C cancels the current fetch before any pending storage mutation. Rerun the
same command: backfill resumes inclusively from the last durable per-stream
checkpoint and collapses the boundary duplicate. If a storage error occurs,
verify permissions and free space before retrying. Do not move a checkpoint
forward by hand.

## Recorder

Ctrl-C requests a clean recorder shutdown, flushes accepted normalized rows,
and prints final diagnostics. Each invocation starts a new session rather than
appending uncertain state to an old one.

- On a network failure, verify reachability and start a new recording session.
- On a schema/normalization failure, preserve the raw JSONL session. The raw
  frame can be replayed after the parser is repaired.
- On a storage failure, preserve any raw session already written, then verify
  dataset permissions and free space before starting again.

Raw JSONL is the recovery source of truth; normalized Parquet is a rebuildable
projection.

## Predictor and TUI

`predict` is a deterministic, one-shot replay. Fix the reported capture,
network, cutoff, or dataset issue and rerun the same command. A missing realized
settlement is reported as unavailable and does not invalidate the prediction.

The TUI tails the existing recorder JSONL incrementally and retries refresh
errors without opening a second network path. Quit with `q`, Escape, or Ctrl-C;
raw terminal mode, the alternate screen, and cursor visibility are restored on
normal exit and error unwinding. If stdout is redirected, use
`predict --output json` instead.

## Files that should remain immutable

Do not repair a capture by deleting malformed lines or rewrite Parquet files in
place. Copy the affected artifacts for investigation, retain the originals,
and let replay/backfill produce a new derived result. This preserves provenance
and makes failures reproducible.
