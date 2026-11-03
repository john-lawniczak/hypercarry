# Release bundles and passive installation

Release tooling is `tools/release_bundle.py` (Python 3.10+ standard library).
It builds from a **clean checkout**, records exact source/lockfile/build identity,
and packages binaries, public documentation, examples and service templates.
It never includes private Git history, untracked configuration, keys, datasets,
local evidence or maintainer-only files. Packaging is not release approval.

## Build and verify

Build natively on the Linux distribution/architecture you intend to support,
with the Rust toolchain and system dependencies installed. Cross builds require
the appropriate Rust target and linker. A target triple alone does not promise
compatibility with an older glibc or every Linux distribution; test the resulting
bundle on the supported host image. The initial reference deployment is Linux
GNU on x86_64 or aarch64. macOS bundles may be built for local verification, but
cannot be installed into a live Linux system through this installer.

```sh
python3 -B tools/release_bundle.py build --output /tmp/hypercarry-candidate.tar.gz
```

The command runs `cargo build --workspace --all-features --release --locked --target TARGET`
for the selected target (native host by default). It always passes the target
explicitly, overriding inherited Cargo target settings, and packages only that
target's output directory. It returns the archive path,
SHA-256, full source commit and target. An optional `--target` selects a configured
cross target. The output must be outside the checkout and must not already exist.

The manifest records each file's SHA-256, size and mode; archive headers are
normalized. Repacking identical staged bytes produces the same archive, but
this is not a claim that Rust builds on unrelated machines are bit-reproducible.
The source must remain clean and at the same commit throughout the build.

Obtain the expected archive checksum through your independently verified release
channel. A checksum shipped by an untrusted party alongside an archive is not
authentication. Verify and stage without root or network calls:

```sh
python3 -B tools/release_bundle.py verify \
  --bundle /tmp/hypercarry-candidate.tar.gz --sha256 EXPECTED_SHA256
python3 -B tools/release_bundle.py stage \
  --bundle /tmp/hypercarry-candidate.tar.gz --sha256 EXPECTED_SHA256 \
  --destination /tmp/hypercarry-reviewed
```

Verification hashes a private snapshot before parsing/extracting it, so replacing
or modifying the original archive cannot swap bytes after verification. It
rejects checksum/manifest mismatches, unlisted/duplicate paths, traversal,
symlinks/hard links, special files, unsafe modes and oversized archives. Staging
requires an absent destination and manually extracts only verified regular files.

## Install without enabling trading

Create the dedicated service identity on each intended host; do not give the
executor host permission to sign with KMS. For example, after reviewing your
Linux account policy:

```sh
sudo useradd --system --no-create-home --shell /usr/sbin/nologin hypercarry-canary
sudo python3 -B tools/release_bundle.py install \
  --bundle /tmp/hypercarry-candidate.tar.gz --sha256 EXPECTED_SHA256 \
  --role executor
```

Use `--role recovery` on the independent signer/watchdog host. Both roles install
all reviewed binary bytes (so offline doctor/hash checks can inspect the same
release), but only their role's systemd units. Runtime/config/monitoring directories
are 0700 and belong to `hypercarry-canary`; binaries are root-owned and executable.
Before writing installation files, the installer requires the selected role's
core executable binaries and systemd units. It checks native Linux GNU target
identity, existing ownership/modes,
and rejects symlinked installation paths. It does not create a login, enable or
start units, call `daemon-reload`, overwrite existing differing files, create live
configuration, or touch journals, nonce reservations, STOP or release evidence.

Use `--root /ABSOLUTE/EXISTING/STAGING_ROOT` without root to inspect/test a passive
installation tree. This is not a live installation and does not validate actual
cloud identities or service permissions. The staging root must be canonical.

Prepare actual reviewed configuration using [setup diagnostics](setup-diagnostics-v1.md).
Examples are installed under `/usr/share/doc/hypercarry/docs/`; they are never
copied into `/etc` as an approved live configuration. Only after independent
review, operational exercises and release approval should you reload systemd and
start the appropriate services according to [the launch runbook](mainnet-launch-runbook.md).

## Upgrades and public source identity

The installer accepts identical existing files but refuses to replace a differing
installation. Stage upgrades separately, resolve all live orders/positions and
STOP/retire the previous release through the recovery runbook before changing
artifacts or configuration. Preserve journals and consumed nonce/placement state;
never use deletion as an upgrade or retry mechanism. New source/binaries/config
require new hashes, review and approval.

Build public release artifacts from the final reviewed **public** commit. A
filtered public export has a different Git commit from the private repository;
its embedded source identity and release evidence must name the public commit.
Retain the review mapping between the trees. Do not publish a private-build
binary and pretend its identity belongs to the exported source commit.
