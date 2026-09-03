# Funding prediction contracts v1

M4 is an offline, deterministic baseline. It consumes immutable M3 raw capture,
uses exact decimal arithmetic, and can join its result to M1 settled-funding
Parquet. It never uses wallet, account, or order-placement data.

## Formula

For each reconstructed oracle and bid/ask impact-price sample:

```text
premium = (max(impact_bid_px - oracle_px, 0)
         - max(oracle_px - impact_ask_px, 0)) / oracle_px

F_8h   = average_premium
       + clamp(0.0001 - average_premium, -0.0005, 0.0005)
F_hour = clamp(F_8h / 8, -0.04, 0.04)
```

Official WebSocket `activeAssetCtx` updates supply the latest oracle, while the
interleaved full-depth `l2Book` updates supply price/size levels. Replay computes
the average bid/ask execution prices for the documented 20,000 USDC BTC/ETH
impact notional or 6,000 USDC other-asset notional. A REST-shaped context that
already carries `impactPxs` can also produce a sample directly.

These values follow Hyperliquid's documented eight-hour interest/clamp
convention, hourly payment conversion, five-second premium sampling, and hourly
funding cap. `rust_decimal::Decimal` is used throughout. The exchange-reported
premium is retained for diagnostics but the baseline recomputes premium from
the three prices.

## Sampling and partial-hour metadata

A prediction targets an exact UTC-hour boundary and uses the preceding
half-open hour `[settlement - 1h, settlement)`. Its `as_of_ms` cutoff must be in
that hour and strictly before settlement. Samples after the cutoff are ignored,
including when a full capture is replayed during a historical backtest.

Reconstructed updates are collapsed into UTC-aligned five-second slots. The latest receive
timestamp in a slot wins; identical records at an identical receive timestamp
deduplicate, while conflicting records at that timestamp fail closed. This
prevents a burst of WebSocket updates from overweighting one protocol sampling
slot.

Prediction JSON schema version 1 reports:

- the arithmetic mean of reconstructed premiums and predicted hourly rate;
- observed and expected five-second slots through the cutoff;
- `coverage_ratio = observed_slots / expected_slots_so_far`, capped at 1;
- `hour_progress_ratio = elapsed_window_ms / 3,600,000`;
- `confidence_ratio = coverage_ratio * hour_progress_ratio`.

Confidence is a deterministic completeness score, not a probability or model
calibration claim.

## Causal evaluation and official benchmark

Realized `fundingHistory` is the only ground truth. A prediction may join only
to the same coin and canonical UTC settlement hour, and its generation time
must precede settlement. The resulting evaluation reports signed error
(`predicted - realized`) and absolute error.

An official `predictedFundings` value may be attached only with its observation
timestamp at or before the prediction cutoff and the same target settlement.
Typed venue predictions are normalized from their declared funding interval to
an hourly rate; a missing or zero interval is rejected. The benchmark is not
used by the baseline. When realized funding exists, its signed and absolute
errors are reported separately for comparison.

`walk_forward` orders hourly cases by settlement, independently applies each
case's cutoff, and emits trailing mean signed error and mean absolute error over
a caller-selected non-zero window. Mixed coins and duplicate settlements are
rejected. No statistical or machine-learning model is included in v1; one
should be considered only after it improves on this baseline out of sample.

## CLI

The offline command replays an M3 raw capture and checks its network identity:

```sh
cargo run -p hypercarry-cli -- predict \
  --network testnet \
  --coin BTC \
  --capture data/raw/schema_version=1/network=testnet/session.jsonl \
  --settlement-ms 1787619600000 \
  --as-of-ms 1787619300000 \
  --dataset data \
  --output json
```

If the settled dataset already contains the target hour, output includes the
realized evaluation. Otherwise it returns the partial-hour estimate and states
that realized error is unavailable. `--official-rate` and
`--official-observed-at-ms` must be supplied together.

Configuration precedence remains flags, environment, JSON config, then safe
defaults. Predictor-specific environment variables are `HYPERCARRY_CAPTURE`,
`HYPERCARRY_SETTLEMENT_MS`, `HYPERCARRY_AS_OF_MS`,
`HYPERCARRY_OFFICIAL_RATE`, and `HYPERCARRY_OFFICIAL_OBSERVED_AT_MS`.
