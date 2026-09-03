# Hyperliquid testnet execution v1

M8 is compiled only with `--features testnet-execution`. The default workspace
and CLI remain read-only and do not link the exchange transport.

## Safety boundary

`HyperliquidTestnetExecutor` can be constructed only with:

- `HyperliquidTestnetConfig`, which accepts no endpoint and therefore cannot be
  redirected to mainnet;
- the exact acknowledgement `I ACKNOWLEDGE HYPERCARRY TESTNET EXECUTION`;
- an isolated `HyperliquidL1Signer` whose typed network is `testnet`, whose key
  alias is safe to audit, and whose signing address is explicitly pinned;
- proactive request throttling and a bounded retry policy; and
- caller-provided metadata, risk snapshot, asset-index, journal, and clock
  providers.

Placement resolves/quantizes the intent and records an explainable risk
decision before it constructs signing material. A rejection never calls the
signer or transport. An allowed GTC limit order includes the durable 128-bit
`cloid`, a monotonic nonce, and a bounded `expiresAfter`. The signature provider
receives the explicit action, nonce, expiry, and typed network and returns the
complete signed request. Hypercarry validates exact field preservation, a null
vault, and the signature shape before transport; it never loads a private key.

`hypersdk-signer` is a nested, default-off feature that pins `hypersdk` 0.2.15
inside the execution crate. The host constructs and protects the signer, then
passes it to `HypersdkTestnetSigner`; there is deliberately no environment,
file, or raw-key loader. The trading account and authorized signer are pinned
separately because a Hyperliquid API/agent wallet normally has a distinct
address. The host remains responsible for independently verifying that agent
authorization on the trading account.

## Submission and reconciliation

The HTTPS transport is fixed to `https://api.hyperliquid-testnet.xyz`, uses
Rustls, bounded connect/request timeouts, and a 1 MiB response limit. Placement
acknowledgements distinguish resting, filled/partial, and rejected outcomes.
Cancellation uses `cancelByCloid` and remains `cancel_pending` until private or
REST state resolves the fill/cancel race.

If a request may have crossed the transport boundary, the state becomes
`submission_uncertain` and the executor queries `orderStatus` with the same
`cloid` before returning. It never blindly resubmits an uncertain action.
Rate-limit delays and known-before-write retry delays are returned explicitly
to the host so sleeping/cancellation remains under operator control.

## Private lifecycle

`connect_testnet_private_stream` opens a dedicated official testnet WebSocket
and sends only account-scoped `orderUpdates` and `userFills` subscriptions. It
services ping/pong and emits an explicit disconnect boundary. The host must
reconnect and REST-reconcile every managed order after any disconnect.

Order updates and both snapshot/live fills normalize to `PrivateEvent`. Trade
IDs and deterministic order-update identities are durably journaled, so
duplicate snapshot frames do not double-count after reconnect or restart.
Cumulative fills, venue order identity, state, and timestamps still pass the M7
state machine; invalid regressions fail closed.

## Verification

The opt-in deterministic lifecycle suite is:

```sh
cargo test -p hypercarry-execution --features testnet-execution --locked
cargo test -p hypercarry-execution --features hypersdk-signer --locked
```

It covers place/rest, private partial fill, duplicate fill, cancel, REST
reconciliation, restart, venue rejection, kill-switch/risk rejection before
signing, uncertain submission, malformed private data, and a fill-winning
cancel race. The SDK test additionally validates and recovers deterministic
order/cancel signatures. No normal or feature test sends a live order.
Credentialed live sessions must follow the operator runbook and be recorded as
post-M9 release evidence.
