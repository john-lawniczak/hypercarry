# Signing migration — fresh review required

**Mainnet remains closed.** The user authorized migrating the signing integration
to crypto-bigint 0.7.10, conditional on fresh security review. This document is
an engineering handoff, not that independent review or a release approval.

## Implementation and compatibility

`hypercarry-signing` replaces HyperSDK 0.2.15 and Alloy's signing umbrella.
It uses k256 0.14.0, elliptic-curve 0.14.3 and an exact crypto-bigint 0.7.10
constraint. Version 0.7.6 is yanked. No vulnerability waiver or patched fork was
introduced. Alloy's public primitives and Solidity EIP-712 encoder remain;
their k256/secp256k1 features are disabled. Curve arithmetic, deterministic
signing and recovery use supported RustCrypto implementations.

Cargo.lock still records **inactive optional** legacy k256 0.13 / elliptic-curve
0.13 / crypto-bigint 0.5 entries declared by Alloy's public-type crates. These
are absent from the resolved build graph, including all workspace features and
all targets. Do not interpret presence in the lockfile as a compiled dependency,
or claim every lockfile entry is patched. `cargo deny` bans activation of those
versions and the old signing SDK. A scanner that inspects every lockfile package
without feature resolution may still flag these entries; provide the resolved
graph to the reviewer. Removing even dormant registry declarations would require
replacing the remaining public-type/EIP-712 libraries or upstream changes.

The action codec supports limit Gtc/Ioc/Alo orders with grouping `na`, ordinary
cancel/cancelByCloid and scheduleCancel. It rejects unknown fields, trigger
orders, builders, fast cancels, transfers, leverage changes and vaults. Service
policy still restricts live signing to the exact reviewed canary and recovery
actions. The codec itself is not an authorization policy. Decimal normalization,
zero-cloid omission and MessagePack field order preserve the prior wire contract.
Absent scheduleCancel time is encoded as null, matching the prior Rust SDK;
mainnet service policy continues to forbid disarming the dead-man switch.

Signature recovery rejects invalid scalars, high-S values and invalid wire
recovery IDs. The KMS boundary continues to normalize DER signatures to low S
before attempting both Ethereum parities against the pinned public address.
Network source (`a` / `b`), nonce and expiry remain in the signed digest.

Embedding API change: use `LocalTestnetSigner<S>` with the new `DigestSigner`
trait (implemented for caller-owned modern k256 `SigningKey`). The former Alloy
signer trait and `HypersdkTestnetSigner` type are removed. `local-signer` is the
new default-off feature; `hypersdk-signer` remains only a deprecated feature
alias. The production provider protocol and configuration formats are unchanged.
The local testnet adapter explicitly pads r/s to 32-byte hexadecimal strings,
including signatures with leading zeroes, as required by provider validation.
No production private-key loader was added. The CLI and supervisor do not depend
on the signing crate in their normal graphs.

## Reproducible evidence

`crates/hypercarry-signing/tests/fixtures/legacy-vectors.json` contains 28 cases
captured **before** replacing HyperSDK 0.2.15 / Alloy 2.5.0. Its generator is
retained as a noncompiled `.rs.txt` fixture, not a dependency on the old SDK.
Cases cover three order TIFs, both sides, reduce-only IOC, high asset IDs,
normalized decimals, omitted cloid, both cancellation kinds, schedule arm/disarm,
both networks, and presence/absence of expiry. Fixture private keys are public,
unfunded test values; never use them for any account.

All 28 action payloads, hashes and deterministic signatures match the new Rust
implementation. Hashes and signatures also match the official
[Hyperliquid Python signing implementation](https://github.com/hyperliquid-dex/hyperliquid-python-sdk/blob/master/hyperliquid/utils/signing.py)
from published SDK 0.24.0. Negative tests cover context mutations, invalid
scalars/recovery parity, high S, malformed/unsupported actions and expiry overflow.
Existing mainnet, provider, watchdog and KMS tests exercise the migrated callers.

Final local verification: **301 passed, zero failed, four live tests ignored**;
strict Clippy, formatting, all-feature release build, default-feature checks,
Rust 1.94.1 checks, supply-chain policy and cargo-deny passed. One subprocess
test timed out during concurrent compilation and passed on rerun; the local
Unix-socket test required execution outside the sandbox. No live order or
credentialed custody exercise was performed.

From the repository root:

```sh
cargo test -p hypercarry-signing --locked
uv run --no-project --with hyperliquid-python-sdk==0.24.0 python tools/verify-signing-vectors.py
cargo tree --workspace --all-features --target all --locked -i crypto-bigint@0.7.10
cargo tree --workspace --all-features --target all --locked -i crypto-bigint@0.5.5
cargo deny check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo build --workspace --all-features --release --locked
cargo +1.94.1 check --workspace --all-targets --all-features --locked
```

The old-version inverse-tree command must print no dependency path. The Python
check is offline after package installation and only signs public test fixtures.
Do not regenerate expected fixtures using the implementation being tested.

## Required reviewer and operator work

1. Have an independent reviewer inspect the new crate and every migrated caller:
   testnet adapter, mainnet verification, provider, watchdog, Foundry provider and
   KMS DER/recovery boundary. Review serialization order, canonicalization,
   domain/nonce/expiry binding, malformed inputs, feature isolation and dependency
   bans. Include inactive lockfile entries explicitly in the review scope.
2. Freeze a clean source commit, Cargo.lock and production Linux binaries. Save
   the complete feature graph, current dependency audit, tests and build results.
   An earlier security report cannot cover the new source or binaries.
3. Rebuild and repin provider/AWS CLI/service artifacts and configuration digests;
   repeat credentialed testnet signing, cancellation, scheduled cancellation,
   restart/replay and partial-fill recovery exercises using these artifacts.
   Test the actual KMS custody path and independent watchdog on the target hosts.
4. Put the new report reference in `reviewed_bundle.execution_security_review`,
   retain required independent session attestations and rollback evidence, then
   prepare new config/gate/evidence digests. Obtain an explicit human release
   decision for that exact bundle. Do not copy earlier approval into the new one.
5. Follow [the launch runbook](mainnet-launch-runbook.md) for provisioning, probes,
   health/failure evidence and the single-canary operation. All existing runtime
   gates remain closed until their evidence and operational conditions pass.
