# Recording host deployment

Stands up continuous mainnet funding capture on an always-on host: persistent
storage, restart behaviour, disk monitoring, and stale-data alerting.

This host is read-only. It holds no key, no credential, and no execution
capability, and the `hypercarry` CLI has no mainnet execution feature to gain
one. Keep it that way: the analytics host and any future signer host must not be
the same machine.

## What actually keeps the carry dataset current

The one thing to understand before deploying:

| | Writes settled funding? | Needed for `apr` / `pnl` / `predict`? | Disk |
|---|---|---|---|
| `hypercarry backfill` | **Yes — only source** | **Yes** | One row per coin per hour |
| `hypercarry record` | No | No | Every book update |

`backfill` is the unit that matters. `record` captures a different layer — live
L2 book and asset-context frames for basis and prediction research — and writes
orders of magnitude more data. It is installed but not enabled by default.

This is worth stating because it is easy to read "continuous recording" as "run
the recorder". Carry P&L would keep working with the recorder switched off, and
would silently stop being current if the backfill timer failed.

## Components

| Unit | Kind | Purpose |
|---|---|---|
| `hypercarry-backfill@<COIN>.timer` | hourly | Keeps settled funding current for one coin |
| `hypercarry-health.timer` | hourly | Freshness, contiguity, and free space |
| `hypercarry-alert@.service` | on failure | Calls `/etc/hypercarry/alert` |
| `hypercarry-record.service` | optional, always-on | Live book capture |

## Host

An `t4g.small` (2 vCPU, 2 GiB) is comfortable for backfill-only operation; the
work is one small HTTPS request per coin per hour. Add headroom if you enable
the recorder.

Put the dataset on a **separate EBS volume** mounted at `/var/lib/hypercarry`,
not on the root disk. Two reasons: the dataset survives instance replacement,
and a runaway recorder fills its own volume rather than wedging the host.

```sh
# On the instance, once, for a volume attached as /dev/nvme1n1:
sudo mkfs.ext4 -L hypercarry /dev/nvme1n1
sudo mkdir -p /var/lib/hypercarry
echo 'LABEL=hypercarry /var/lib/hypercarry ext4 defaults,noatime,nofail 0 2' \
  | sudo tee -a /etc/fstab
sudo mount -a
```

Clock accuracy matters: settlement freshness is judged against the host clock.
Amazon Linux and Ubuntu on EC2 sync to the Amazon Time Sync Service by default —
confirm with `timedatectl` that NTP is active before trusting the health check.

## Install

Build the binary, then install:

```sh
cargo build --release --bin hypercarry
sudo install -m 0755 target/release/hypercarry /usr/local/bin/hypercarry
sudo dnf install -y jq          # or: sudo apt-get install -y jq
sudo deploy/install.sh
```

`install.sh` creates the `hypercarry` system user, the state directory, the
config and the units. It enables nothing — a host should not start recording
because a script ran.

Then:

1. Edit `/etc/hypercarry/hypercarry.env`. At minimum set `HYPERCARRY_NETWORK`,
   `HYPERCARRY_DATASET`, and `HYPERCARRY_COINS`.
2. Replace `/etc/hypercarry/alert` with real delivery, and **prove it**:
   ```sh
   sudo systemctl start hypercarry-alert@test.service
   ```
   An alert hook that silently does nothing is worse than none, because the
   timers keep looking healthy while nobody is being told.
3. Seed history once per coin, so the first health check has something to judge:
   ```sh
   sudo -u hypercarry /usr/local/bin/hypercarry backfill \
     --network mainnet --coin BTC --days 30 \
     --dataset /var/lib/hypercarry/data --output human
   ```
4. Enable the timers:
   ```sh
   sudo systemctl enable --now hypercarry-backfill@BTC.timer
   sudo systemctl enable --now hypercarry-backfill@ETH.timer
   sudo systemctl enable --now hypercarry-health.timer
   ```

## How gaps repair themselves

Each hourly run re-requests `HYPERCARRY_BACKFILL_DAYS` of history, not just the
newest hour, and merging deduplicates on `(network, venue, coin, settlement
time)`. So any outage shorter than that window is repaired by the next
successful run with no operator action. A conflicting rewrite of a settlement
already stored fails loudly instead of silently winning.

`Persistent=true` on the backfill timer means a run missed while the host was
down fires shortly after boot rather than waiting for the next hour.

The limit is real: an outage **longer** than `HYPERCARRY_BACKFILL_DAYS` leaves a
permanent hole unless someone runs a wider `--days` backfill manually. That is
what the contiguity alert is for.

## Acceptance

### 24 hours of contiguous funding history

```sh
hypercarry apr --network mainnet --coin BTC \
  --dataset /var/lib/hypercarry/data --output json | jq .contiguous_history_hours
```

Must be `>= 24`. This is the unbroken run ending at the newest observation, so
it is a real completeness claim — unlike a row count, which cannot distinguish a
complete day from a week with holes in it.

The human view states both:

```
History          168 hourly observations, all contiguous (7 days)
```

### Recovery after restart

```sh
sudo systemctl restart hypercarry-backfill@BTC.timer
sudo reboot
# after it comes back:
systemctl list-timers 'hypercarry-*'
sudo systemctl start hypercarry-health.service
journalctl -u hypercarry-health.service -n 30 --no-pager
```

The health check must pass, and `contiguous_history_hours` must still be rising.
If you enabled the recorder, also confirm a clean stop finalizes its session:

```sh
sudo systemctl stop hypercarry-record.service
journalctl -u hypercarry-record.service -n 20 --no-pager   # JSON diagnostics
```

The recorder treats `SIGTERM` as cancel-and-drain, so the session's normalized
Parquet is finalized on a normal stop. A hard kill would leave it unfinalized
and require replaying the raw JSONL to recover the projection.

### Alert delivery

```sh
sudo systemctl start hypercarry-alert@test.service
```

Confirm arrival. Then force a real failure end to end — point
`HYPERCARRY_DATASET` at an empty directory, run
`sudo systemctl start hypercarry-health.service`, confirm it fails and the alert
arrives, then put the setting back.

## Operating

```sh
systemctl list-timers 'hypercarry-*'
journalctl -u 'hypercarry-backfill@BTC.service' -n 50 --no-pager
journalctl -u hypercarry-health.service -f
du -sh /var/lib/hypercarry/data
```

Thresholds live in `/etc/hypercarry/hypercarry.env`:

- `HYPERCARRY_MAX_SETTLEMENT_AGE_S` — freshness. Default 9000s tolerates one
  missed hourly settlement plus publication delay.
- `HYPERCARRY_MIN_CONTIGUOUS_HOURS` — the completeness gate. Default 24.
- `HYPERCARRY_MIN_FREE_MIB` — disk. Raise it well above one day's growth if you
  enable the recorder.

## If you enable the recorder

Watch the first day's growth rather than trusting an estimate:

```sh
du -sh /var/lib/hypercarry/data/raw
```

Each start opens a new session file, so restarts produce a sequence of sessions
rather than one growing file — that is what keeps each session's receive order
replayable. Raw JSONL is the replay source of truth: pruning it discards the
ability to rebuild the normalized layer for those days, so
`HYPERCARRY_RAW_RETENTION_DAYS` is unset (keep everything) by default.

## Taking the first carry position

Once 24 hours of contiguous history is in place, `TODO.md` Track 1 calls for a
small manual position on the Hyperliquid interface, recorded as a trade document
and evaluated with `hypercarry pnl`.

Note when `window_fully_covered` can be true: the position must be **closed**,
and at least one settlement after its exit must be recorded. It reads false
while the position is open, and in the gap between closing and the next
settlement. That is the check working — it requires observations bracketing the
window plus an unbroken hourly sequence between them, so it can detect a
settlement missing *inside* the window, which a span check cannot.
