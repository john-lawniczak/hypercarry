# Scoped MCP interface v1

`hypercarry-mcp` is a separate local stdio process. It implements MCP
2025-11-25 initialization, ping, tools/list and tools/call over newline-delimited
JSON-RPC. stdout contains protocol messages only. Launch it separately for each
user/client and capability set. No remote listener is opened.

## Build and integration

```sh
cargo build --release -p hypercarry-cli -p hypercarry-mcp --locked
```

Copy `mcp-config.example.json`, choose explicit network and coin allowlist, and
set absolute paths. Pin the actual analytics executable's SHA-256. Storage
paths must be canonical directories, without symlinks. Register capture IDs in
`captures`; clients cannot supply arbitrary filesystem paths or executables.

A client configuration can launch:

```json
{
  "mcpServers": {
    "hypercarry": {
      "command": "/opt/hypercarry/bin/hypercarry-mcp",
      "args": ["--config", "/etc/hypercarry/mcp.json", "--scope", "read"]
    }
  }
}
```

For local writes, use `--scope read,write`. Scopes are enforced at discovery
and dispatch, not just described with MCP annotations. Tool arguments cannot
change network, paths, executables or scopes. Environment configuration is not
inherited by analytics subprocesses. The process has no wallet or signing
stack in its dependency tree.

| Scope | Tools | Effects |
|---|---|---|
| read (default) | get_funding_snapshot | Public venue reads |
| read | get_funding_history, get_funding_apr, get_dataset_health | Local dataset reads |
| read | evaluate_carry_trade, get_prediction | Local deterministic evaluation |
| write | backfill_funding | Public reads and dataset updates |
| write | save_trade | Validate and save a new trade; never overwrite |

Trade access is a separate launch capability: `--scope read,write,trade
--enable-mainnet`. Write alone never permits trading. Account custody remains
external. Trading tools are `submit_reviewed_canary` and
`recover_reviewed_canary`; their only argument is `{approval_id}`. They cannot
change the order, account or release configuration.

## Tool contracts

- `get_funding_snapshot`, `get_funding_apr`, `get_dataset_health`: `{coin}`.
- `get_funding_history`: `{coin,start_ms,end_ms,limit?}`. Inclusive range,
  default 100/max 1000 records. Use `next_start_ms` for the next page. Coverage
  checks the entire requested window including both boundary brackets, not
  only the returned page. No history call implicitly fetches missing data.
- `evaluate_carry_trade`: `{coin,trade}` or `{coin,trade_id}`. `trade` is the
  existing schema-v1 CLI trade document. A temporary evaluation file is removed
  afterward. Funding valuation remains entry-price/fixed-price approximation.
- `get_prediction`: `{coin,capture_id,settlement_ms,as_of_ms}`. Reuses the causal
  CLI predictor and its cutoff checks.
- `backfill_funding`: `{coin,days}` with 1..365 days.
- `save_trade`: `{coin,trade_id,trade}`. Evaluates with the actual CLI before
  persisting. The stored wrapper binds network/coin; reads reject a mismatch.
  IDs contain only 1..64 ASCII letters, digits, underscores and hyphens.

Every tool result, including tool failures, has schema version, network,
source `observed_at_ms` (null when unknown), separate response time, units,
coverage, valuation assumptions and provenance. Monetary/rate values remain
base-10 strings. Unknown coverage is null, never implicitly complete. The
analytics executable hash and trade-document hash identify calculation inputs.
History records retain ingestion time, original request range and source class.
Protocol handshake and JSON-RPC protocol errors are not market-data envelopes.

Prediction confidence is **data completeness**: sampling coverage multiplied
by elapsed fraction of the funding hour. It is not probability of profit,
statistical calibration or a trading recommendation. Simple annualized APR
is also not a promised future return. Position output uses the trade's supplied
fees and reports returns on perp entry notional, not total strategy capital.

## Operational bounds

Messages and command output are capped at 1 MiB. Commands have a configured
100ms..120s deadline and are killed/reaped when interrupted or timed out.
A timed-out write may have completed; inspect the dataset or saved trade before
retrying. Notifications never execute tools. Invalid arguments and ungranted
capabilities fail before a child process is started. Operations are serialized
within one stdio session. Use one writer per dataset; cross-process concurrent
dataset ingestion is not introduced by this interface.

MCP hints describe effects; they do not enforce permission. The process scope
checks enforce the boundary. Client/UI approval settings remain an additional
control and cannot expand the server's capabilities.

References: [MCP transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports),
[MCP tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools).

## One-use mainnet consent

Add an `execution` object to the server configuration:

```json
{
  "executor_bin": "/opt/hypercarry/bin/hypercarry-executor",
  "executor_sha256": "REPLACE_WITH_SHA256_OF_REVIEWED_EXECUTOR",
  "config": "/etc/hypercarry-canary/config.json",
  "evidence": "/etc/hypercarry-canary/release.json",
  "approval_directory": "/var/lib/hypercarry-approvals"
}
```

The approval directory must be canonical, private (0700), and outside MCP's
writable dataset/trade roots. Configuration, evidence and executor artifacts
must also be outside those roots. Build the executor with `mainnet-execution`,
from a clean committed tree, and complete its release preparation first.

The operator issues approval in a terminal, after viewing the exact action:

```sh
hypercarry-executor approve-mcp \
  --config /etc/hypercarry-canary/config.json \
  --evidence /etc/hypercarry-canary/release.json \
  --output /var/lib/hypercarry-approvals/canary-001.json \
  --operation run --ttl-seconds 60
```

This displays account, side, size, market, price and runtime digest and requires
`ENABLE HYPERCARRY MAINNET CANARY`. For recovery use `--operation recover` and
`RECOVER HYPERCARRY MAINNET ORDERS`. The maximum approval lifetime is 300 seconds.
The tool can then consume `{ "approval_id": "canary-001" }`.

Approval binds network, operation, runtime and evidence digests. An atomic,
non-overwriting hard-link claim retains `canary-001.consumed`; only one caller
can win, and a crash after claiming does not permit replay. Never remove claim
records to retry an uncertain order. The executor requires its explicit mainnet
flag even with an approval, re-verifies clean build/artifact identity and static
evidence, then follows the same live authorization, risk, signer, kill-switch,
watchdog and journal path as the manual CLI. MCP never fabricates release
approval or invokes a wallet itself. A recover approval never permits entry.

The approval is a protected local filesystem capability, not a signed portable
credential. The OS account controlling that directory is trusted; do not expose
it as a remotely writable directory or use one shared identity for untrusted
users. Local-write tools cannot issue approvals. Use separate MCP processes and
OS identities when permissions differ between users.
