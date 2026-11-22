# Local operational notifications

`hypercarry-notify` is a separate local worker with no signer or wallet access.
It imports complete JSONL records containing a typed `notification` at the root
or under `event`. Cursor advancement and enqueue are one durable state update.
The producer journals the notification together with the trading transition;
provider latency or failure therefore never enters the trading critical path.

## Build and configure

```sh
cargo build -p hypercarry-notifications --features delivery --release
install -d -m 700 "$HOME/.config/hypercarry/notifications"
install -d -m 700 "$HOME/.local/state/hypercarry-notifications"
```

Create a mode-0600 config outside this repository. Replace the example absolute
paths with your own. Start with console delivery, which needs no credentials:

```json
{
  "schema_version": 1,
  "state_dir": "/absolute/private/notification-state",
  "sources": [{"id": "recovery", "path": "/absolute/private/recovery.jsonl"}],
  "routes": [{
    "id": "terminal", "minimum_severity": "info", "kinds": [],
    "provider": {"type": "console"}
  }],
  "max_attempts": 3,
  "retry_ms": 2000,
  "poll_ms": 5000,
  "delivery_timeout_ms": 3600000,
  "sends_per_minute": 10,
  "critical_reminder_ms": 300000,
  "max_reminders": 2
}
```

Source journals must be private regular files and remain append-only. A missing
or malformed source is reported by its configured ID; its cursor is retained.
Healthy sources continue importing and existing deliveries continue draining.
Continuous mode reports degraded ingestion on stderr and retries on the next
pass; `--once` drains existing work, then exits nonzero if any source failed.
An empty `sources` list permits manual `enqueue --event /private/event.json`.
Config and token files must be private regular files; state directories must be
mode 0700. On Unix, reads and lock opens use `O_NOFOLLOW` and validate the opened
inode's type, permissions, link count and size. Nonblocking opens reject FIFOs
without waiting for a writer. Keep all parent directories under your control;
this is not a sandbox against a compromised local account. No addresses, keys
or recipient data belong in tracked configuration.

For email, add a route with minimum severity `warning` (or `info`) and:

```json
{"type":"resend", "from":"alerts@example.com", "to":"operator@example.com",
 "api_key_file":"/absolute/private/resend-token"}
```

For SMS, use a `critical` route and an opted-in recipient:

```json
{"type":"twilio", "account_sid":"AC00000000000000000000000000000000",
 "from":"+12025550101", "to":"+12025550102",
 "auth_token_file":"/absolute/private/twilio-token"}
```

These are illustrative destinations, not configured accounts. Store the actual
provider token alone in the named mode-0600 file. Resend needs a verified sender
and permission to send/read email; Twilio needs an enabled sender and permission
to send/read messages. Provider setup and a real delivery test are separate from
repository tests. Nothing is subscribed or sent by installing this crate.

```sh
# Inspect first; these commands send no messages.
hypercarry-notify --config /private/notify.json status
# Console routes work without this flag. Email/SMS require explicit opt-in.
hypercarry-notify --config /private/notify.json run --once --enable-delivery
# Omit --once for continuous polling under a local process supervisor.
hypercarry-notify --config /private/notify.json ack --event-id EVENT_ID_FROM_STATUS
```

Each worker pass holds an exclusive state lock, released between passes.
Status/ack refuse a competing writer; retry between passes if busy. Run one worker
per state directory. Use launchd/systemd restart policies and private logs; no
service is installed automatically. A local worker cannot alert when its machine
is powered off or disconnected: an independently hosted missed-heartbeat monitor
is still needed for that failure mode. Console output is not a desktop push.

## Portfolio-observation producer

`account ... portfolio --archive-dir <dir>` is also a source producer, with no
dependency on `hypercarry-notify` and no HTTP/signing crate in its graph. Every
archive attempt, success or failure, appends secret-free `NotificationEvent`
records to `<dir>/notifications.jsonl` — the same JSON-lines shape consumed
above — whenever something actually changed since the previous attempt, never
on every poll:

- A position losing full reduce-only TP/SL coverage: `protection_missing`.
- A position regaining it: `protection_verified`, which resolves the earlier
  alert through the existing matching logic with no additional configuration.
- A previously tracked position disappearing from the snapshot (closed):
  `position_closed`.
- The read itself failing, including the existing staleness checks in
  `core::portfolio`: `state_unknown`, labeled with the synthetic coin
  `__account__`. A read succeeding again clears this silently so a later
  failure raises a fresh alert instead of staying suppressed.

Point an `Outbox` source at that file (`{"id": "portfolio", "path":
"<dir>/notifications.jsonl"}`) to deliver these through the ordinary pipeline
above. Orphan trigger orders are reported in the portfolio output itself and
are not separately alerted here.

A separate, independent liveness check detects a collector that stopped
running entirely — crashed, wedged, or never started — which the collector
itself cannot report:

```sh
hypercarry portfolio-heartbeat-check --network mainnet \
  --archive-dir /absolute/private/archive --max-age-ms 3600000
```

This performs no network or account access. It compares `<dir
>/.portfolio-heartbeat.json`'s own filesystem modification time (not its
content, so a copied or replicated file cannot forge it) against
`--max-age-ms`, appends a `heartbeat_lost` alert on the transition into
staleness (once, not on every check, and not auto-resolved — it requires
human acknowledgement through the outbox like any other state-unknown
condition), and exits nonzero while stale. Run it from a separate cron or
systemd timer so it keeps checking even if the collector's own timer stops
firing.

## Delivery and failure semantics

- Stable event and delivery IDs deduplicate journal replay. Conflicting duplicates,
  changed journal prefixes fail closed for the affected source, without advancing
  its cursor. Regressed clocks and config drift stop the worker.
- Email retries reuse one idempotency key, capped at 23 hours, within Resend's
  documented 24-hour retention. SMS has no assumed idempotent-send contract:
  a timeout or a crash after reservation becomes **uncertain**, not an automatic
  resend. HTTP 429 can retry; other ambiguous responses remain uncertain for SMS.
- HTTP acceptance is **not delivery**. Polling checks provider receipt identity
  and delivered/failed status. `sent` remains pending. Receipt timeout becomes
  uncertain. Human acknowledgement is a separate durable field.
- Critical events have bounded, separately identified reminders until acknowledged
  or resolved. A reminder can follow an uncertain original; it is explicitly
  marked and is not a silent retry. Restart can catch up overdue reminders, still
  subject to the per-route rate limit. Set `max_reminders: 0` to disable them.
- Position closure resolves earlier alerts for that trade/market/network;
  protection verification resolves missing-protection alerts. Neither fabricates
  human acknowledgement or a delivery receipt.
- Attempts, exponential backoff, delivery age, per-route sends per minute and
  work per pass are bounded. Events older than `delivery_timeout_ms` are not sent.
  Production refreshes time before each request and after each response; retry
  delays start at response completion. A slow provider cannot extend the age
  allowance of later queued events in the same pass.
  State is capped at 10,000 events/deliveries and 32 MiB. Capacity exhaustion is an
  ingestion error; existing deliveries still drain and new reminders are deferred
  at capacity. Archive/migrate with the worker stopped. Do not
  delete state and replay old journals as a retry mechanism.
- Credentials, raw provider bodies and arbitrary user-supplied message text are
  never logged. Templates contain event kind, network, trade label, market,
  remaining quantity, timestamp and a short event ID. Labels themselves should
  not contain private personal information. HTTPS origins are fixed, redirects
  and implicit HTTP retries are disabled, and requests have deadlines.

The default library feature set contains no HTTP client or key access. Tests use
simulated senders and provider-response fixtures; they do not contact recipients.
The persisted outbox records current status/attempt counters and receipts, not a
complete provider billing or delivery-attempt audit trail.

Sources: [Resend idempotency](https://resend.com/docs/dashboard/emails/idempotency-keys),
[Resend retrieval](https://resend.com/docs/api-reference/emails/retrieve-email),
[Twilio message resource](https://www.twilio.com/docs/messaging/api/message-resource).
