#!/usr/bin/env bash
set -euo pipefail

# Install the Hypercarry recording host. Idempotent: safe to re-run after an
# upgrade, and it never overwrites /etc/hypercarry/hypercarry.env or the
# operator's alert hook once they exist.
#
# Run as root on the host, from a checkout of this repository:
#
#   sudo deploy/install.sh
#
# It installs units and scripts but enables nothing. Enabling is a separate,
# deliberate step, documented in deploy/README.md — a host that starts recording
# because a script was run is a host nobody decided to operate.

readonly USER_NAME=hypercarry
readonly STATE_DIR=/var/lib/hypercarry
readonly DATA_DIR="$STATE_DIR/data"
readonly CONF_DIR=/etc/hypercarry
readonly UNIT_DIR=/etc/systemd/system
readonly DOC_DIR=/usr/share/doc/hypercarry
readonly BIN_DIR=/usr/local/bin

if ((EUID != 0)); then
  echo "install: must run as root" >&2
  exit 1
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# --- Preconditions ------------------------------------------------------------

if ! command -v jq >/dev/null 2>&1; then
  echo "install: jq is required by the health check; install it first" >&2
  echo "install:   dnf install -y jq   # or: apt-get install -y jq" >&2
  exit 1
fi

if [[ ! -x "$BIN_DIR/hypercarry" ]]; then
  echo "install: $BIN_DIR/hypercarry not found." >&2
  echo "install: build it (cargo build --release --bin hypercarry) and install" >&2
  echo "install: the binary there before running this script." >&2
  exit 1
fi

# --- Identity and storage -----------------------------------------------------

# A dedicated system account with no login and no home. The recording host holds
# no credentials; this account exists so that stays structurally true.
if ! id -u "$USER_NAME" >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin "$USER_NAME"
  echo "install: created system user $USER_NAME"
fi

install -d -o "$USER_NAME" -g "$USER_NAME" -m 0750 "$STATE_DIR" "$DATA_DIR"
install -d -o root -g root -m 0755 "$CONF_DIR" "$DOC_DIR"

if ! mountpoint -q "$STATE_DIR"; then
  echo "install: WARNING: $STATE_DIR is not a separate mount." >&2
  echo "install: On EC2 the dataset should live on its own EBS volume, so it" >&2
  echo "install: survives instance replacement and cannot fill the root disk." >&2
fi

# --- Configuration ------------------------------------------------------------

if [[ -e "$CONF_DIR/hypercarry.env" ]]; then
  echo "install: kept existing $CONF_DIR/hypercarry.env"
else
  install -o root -g root -m 0644 \
    "$here/hypercarry.env.example" "$CONF_DIR/hypercarry.env"
  echo "install: wrote $CONF_DIR/hypercarry.env from the example — EDIT IT"
fi

if [[ -e "$CONF_DIR/alert" ]]; then
  echo "install: kept existing $CONF_DIR/alert"
else
  cat >"$CONF_DIR/alert" <<'HOOK'
#!/usr/bin/env bash
set -euo pipefail

# Alert delivery for a failed Hypercarry unit. $1 is the failed unit name.
#
# REPLACE THIS. As written it only writes to the journal, which means the
# failure is recorded exactly where nobody is looking. Send it somewhere a human
# will see out of hours: SNS, PagerDuty, email, a chat webhook.
#
# Then prove it works:
#   systemctl start hypercarry-alert@test.service
# and confirm the message actually arrived.

unit="${1:-unknown}"
logger -t hypercarry-alert "UNDELIVERED ALERT: $unit failed on $(hostname)"
HOOK
  chmod 0755 "$CONF_DIR/alert"
  chown root:root "$CONF_DIR/alert"
  echo "install: wrote placeholder $CONF_DIR/alert — REPLACE IT"
fi

# --- Programs and units -------------------------------------------------------

install -o root -g root -m 0755 \
  "$here/bin/hypercarry-health-check" "$BIN_DIR/hypercarry-health-check"

install -o root -g root -m 0644 "$here/systemd"/* "$UNIT_DIR/"
install -o root -g root -m 0644 "$here/README.md" "$DOC_DIR/deploy-README.md"

systemctl daemon-reload

# --- What to do next ----------------------------------------------------------

cat <<'NEXT'

install: done. Nothing is enabled yet, by design.

  1. Edit   /etc/hypercarry/hypercarry.env      (network, dataset, coins)
  2. Write  /etc/hypercarry/alert               (real delivery, then test it)
  3. Seed the history once, per coin:
       sudo -u hypercarry /usr/local/bin/hypercarry backfill \
         --network mainnet --coin BTC --days 30 \
         --dataset /var/lib/hypercarry/data --output human
  4. Enable the hourly backfill, per coin:
       systemctl enable --now hypercarry-backfill@BTC.timer
       systemctl enable --now hypercarry-backfill@ETH.timer
  5. Enable the health check:
       systemctl enable --now hypercarry-health.timer
  6. Only if you want live book capture, and after reading its warning:
       systemctl enable --now hypercarry-record.service

Verification and the 24-hour acceptance check are in deploy/README.md.
NEXT
