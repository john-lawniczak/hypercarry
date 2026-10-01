#!/usr/bin/env bash
set -euo pipefail

# Reports how far the living documents have drifted behind the code.
#
# The pre-commit hook catches drift at the moment it would be introduced. This
# script measures accumulated drift, so a bypassed hook still leaves a visible
# signal in CI output and in a reviewer's terminal.
#
# Usage:  tools/check-docs-current.sh [threshold]
#
# Exits 0 when every tracked document is within the threshold, 1 otherwise.
# Documents absent from this checkout are skipped: Z-Explainer.md is
# maintainer-only and is stripped from the public mirror.

threshold="${1:-5}"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "check-docs-current: not a git repository; nothing to compare" >&2
  exit 0
fi

# Documents that must track changes under crates/.
docs=("DEV_STATUS.md" "Z-Explainer.md")

status=0
printf '%-18s %-10s %s\n' "DOCUMENT" "BEHIND" "LAST UPDATED"

for doc in "${docs[@]}"; do
  if [[ ! -f "$doc" ]]; then
    printf '%-18s %-10s %s\n' "$doc" "-" "absent from this checkout; skipped"
    continue
  fi

  last="$(git log -1 --format=%H -- "$doc" 2>/dev/null || true)"
  if [[ -z "$last" ]]; then
    printf '%-18s %-10s %s\n' "$doc" "?" "no commit history; skipped"
    continue
  fi

  # Commits that touched crates/ since this document was last written.
  behind="$(git rev-list --count "$last..HEAD" -- crates/ 2>/dev/null || echo 0)"
  when="$(git log -1 --format=%cs -- "$doc")"

  printf '%-18s %-10s %s\n' "$doc" "$behind" "$when"

  if (( behind > threshold )); then
    status=1
  fi
done

echo

if (( status )); then
  cat >&2 <<EOF
warning: a living document is more than $threshold source commits behind.

Each document carries its own "Keeping this file current" checklist; work
through it and update the date at the top. Sustained drift is how a guide
starts describing a repository that no longer exists.
EOF
else
  echo "Living documentation is current within $threshold source commits."
fi

exit "$status"
