# Dependency PR review — 2026-11-03

The five pending dependency updates are useful and were not present in the first
version of public PR #27. Consolidate their exact versions into the private source
and that single public update, then close the standalone PRs as superseded after
the combined public CI passes. Mainnet approval remains closed.

| PR | Update | Review decision |
|---|---|---|
| [#22](https://github.com/john-lawniczak/hypercarry/pull/22) | signal-hook 0.4.4 → 0.4.5 | Include the descriptor close-on-exec correction. The removed pre-API-21 Android fallback is outside the Unix mainnet deployment target. |
| [#23](https://github.com/john-lawniczak/hypercarry/pull/23) | rust_decimal 1.42.1 → 1.43.0 | Include parsing and arithmetic fixes after current financial, wire-format and digest regressions pass. This is the most behavior-sensitive update. |
| [#24](https://github.com/john-lawniczak/hypercarry/pull/24) | clap_complete 4.6.9 → 4.6.11 | Include the zsh completion escaping fix; regenerate against the current lockfile to avoid the stale PR's unrelated Windows dependency changes. |
| [#25](https://github.com/john-lawniczak/hypercarry/pull/25) | reqwest 0.13.4 → 0.13.5 | Include blocking-client timeout and proxy-auth fixes. Re-run the actual supervisor HTTP/WS fault harness. |
| [#26](https://github.com/john-lawniczak/hypercarry/pull/26) | serde_json 1.0.151 → 1.0.154 | Include parser corrections while retaining configuration/signing digest compatibility checks. The old CI failure was not a serde_json test failure. |

Review used the upstream [decimal changes](https://github.com/paupino/rust-decimal/compare/1.42.1...1.43.0),
[reqwest release](https://github.com/seanmonstar/reqwest/releases/tag/v0.13.5),
[completion changelog](https://github.com/clap-rs/clap/blob/clap_complete-v4.6.11/clap_complete/CHANGELOG.md),
[signal-hook changes](https://github.com/vorner/signal-hook/compare/v0.4.4...v0.4.5)
and [serde_json releases](https://github.com/serde-rs/json/releases/tag/v1.0.154).
Version 1.43 removes the optional rkyv 0.7 feature bridge; this workspace does not
enable that serialization feature. Its active decimal configuration remains
`std` plus `serde-with-str`.

## Failed CI and stale lockfile details

[#26's failed run](https://github.com/john-lawniczak/hypercarry/actions/runs/38771538281)
stopped at cargo-deny on four existing rustls advisories:
RUSTSEC-2026-0346, -0347, -0348 and -0349. Its MSRV job passed, but formatting,
Clippy, tests and the release build were skipped. That historical green/red
status is not validation of the current combined graph. The signing/dependency
remediation in #27 already addresses those underlying audit failures.
The other four green dependency runs date from October 8, before the October 15
audit failure, so their green badges do not establish a clean current audit.

#24 also rewired several unrelated Windows dependency edges from windows-sys
0.61.2 to 0.52.0. The combined update preserves the current Windows selections.
It also retains tempfile's existing getrandom 0.4.3 edge instead of accepting an
unrelated resolver downgrade. Cargo's locked checks validate the retained graph.
The only added package identities are the five requested versions; obsolete
optional rkyv-related lockfile entries are removed.

## Regression and release binding

Added a regression through `RuntimeConfig` deserialization for malformed order
prices and quantities with garbage or a second decimal point beyond decimal's
rounding position. These strings must fail parsing, rather than silently become
a different numeric input. Existing fixed configuration digests, decimal
normalization, signing vectors, financial calculations and supervisor signal/
reconnect tests remain the other compatibility checks.

Run the complete workspace tests, strict Clippy, formatting, Rust 1.94.1 and
default-feature checks, release build, Python tooling suite and dependency audit
before publishing the refreshed candidate. Public CI must pass before closing
the superseded PRs. GitHub's required approving review for #27 still applies.

Local result on the final combined graph: 318 Rust tests passed with four live
tests ignored; 24 Python tests passed. Formatting, strict Clippy, MSRV/default
checks, release build, dependency/supply-chain checks and 28 official SDK signing
vectors passed. Public CI is the remaining check on the refreshed PR head.

Keep dependency changes in the private source as well as the public mirror:
otherwise a later snapshot export can undo a public-only Dependabot merge.
Any previously built bundle has an older lockfile hash and must not be reused
as evidence for this update. Build and review final release artifacts from the
eventual public main commit, after the required review and squash merge.
