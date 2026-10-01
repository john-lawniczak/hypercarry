# Security policy

## Supported versions

Hypercarry is pre-release software. Security fixes are applied to the latest
revision of the `main` branch; no released version is currently supported.

The shipped `hypercarry` CLI is read-only. Separate default-off adapters support
testnet execution and release-gated mainnet execution. The mainnet adapter and
manual executor are implemented, but production trading remains unapproved:
independently reviewed testnet evidence, an execution security review, external
operational services, and final release approval are still required. See the
[release gate](docs/mainnet-release-gate-v1.md) and
[integration runbook](docs/mainnet-integration-v1.md).

The 2026-09-03 security review predates the mainnet integration and does not
constitute a security review or approval of that new code.

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use GitHub's
[private vulnerability reporting form](https://github.com/john-lawniczak/hypercarry/security/advisories/new)
and include the details below. That form requires the repository to be
public; while it is private, reach the maintainer directly through the
contact listed on the
[repository owner's GitHub profile](https://github.com/john-lawniczak)
instead.

Include:

- the affected revision and feature flags;
- a minimal reproduction or proof of concept;
- the expected and observed security boundary;
- potential exposure of keys, orders, balances, datasets, or evidence; and
- any suggested remediation, if known.

Avoid placing real private keys, seed phrases, passwords, API credentials, or
sensitive account artifacts in the report. Use redacted fixtures and testnet
identities whenever possible.

Reports will be acknowledged through the private advisory. Disclosure timing
will be coordinated after the impact is understood and a fix is available.

## Scope priorities

High-priority reports include signing-boundary bypasses, mainnet-gate bypasses,
order identity or reconciliation failures, secret leakage, path traversal,
journal/evidence tampering, unsafe arithmetic, and dependency compromise.

