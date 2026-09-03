#!/usr/bin/env bash
set -euo pipefail

lockfile="${1:-Cargo.lock}"

if [[ ! -f "$lockfile" ]]; then
  echo "error: Cargo lockfile not found: $lockfile" >&2
  exit 2
fi

# Rust Security Response Team incident, 2026-08-20:
# https://blog.rust-lang.org/2026/08/20/supply-chain-attack-on-arrayref/
#
# The first three packages were compromised only at the listed versions. The
# remaining packages were malicious at every published version and were removed
# from crates.io.
awk '
  function check_package() {
    compromised_version = \
      (package == "append-only-vec" && version == "0.1.9") || \
      (package == "arrayref" && version == "0.3.10") || \
      (package == "internment" && version == "0.8.7")

    malicious_package = \
      package == "proc-macro1" || \
      package == "proc-macro-en" || \
      package == "aovine" || \
      package == "arone" || \
      package == "aronenao" || \
      package == "tinymember"

    if (compromised_version || malicious_package) {
      printf "error: Cargo.lock contains malicious crate %s@%s\n", package, version > "/dev/stderr"
      found = 1
    }
  }

  /^\[\[package\]\]$/ {
    check_package()
    package = ""
    version = ""
    next
  }

  /^name = "/ && package == "" {
    package = $0
    sub(/^name = "/, "", package)
    sub(/"$/, "", package)
    next
  }

  /^version = "/ && version == "" {
    version = $0
    sub(/^version = "/, "", version)
    sub(/"$/, "", version)
    next
  }

  END {
    check_package()
    exit found
  }
' "$lockfile"

echo "Rust supply-chain deny-list check passed"
