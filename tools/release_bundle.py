#!/usr/bin/env python3
"""Build, verify, stage or install a reviewed bundle; never start services."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import platform
import shutil
import stat
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parent.parent
MAX_FILE = 512 * 1024 * 1024
MAX_TOTAL = 2 * 1024 * 1024 * 1024
ROLE_UNITS = {
    "executor": ["hypercarry-supervisor.service"],
    "recovery": ["hypercarry-signer.service", "hypercarry-watchdog.service"],
}
# Both roles retain the same reviewed binaries: the offline doctor verifies
# signer/watchdog and probe identities even on the other role's host.
REQUIRED_PROGRAMS = [
    "hypercarry-executor", "hypercarry-supervisor", "hypercarry-operations-probe",
    "hypercarry-doctor", "hypercarry-mainnet-signer", "hypercarry-kms-provider", "hypercarry-watchdog",
]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest_file(path):
    with open(path, "rb") as stream:
        return digest_stream(stream)


def digest_stream(stream):
    digest = hashlib.sha256()
    while chunk := stream.read(1024 * 1024):
        digest.update(chunk)
    return digest.hexdigest()


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def safe_name(name):
    path = PurePosixPath(name)
    require(name and not path.is_absolute() and ".." not in path.parts
            and str(path) == name and "\\" not in name, "unsafe archive path")
    return name


def add_file(stage, source, name, mode):
    require(source.is_file() and not source.is_symlink(), f"not a regular source: {name}")
    target = stage / safe_name(name)
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)
    target.chmod(mode)


def write_archive(stage, manifest, output):
    # Exclusive creation prevents accidentally replacing an existing release.
    with output.open("xb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as archive:
                payload = json.dumps(manifest, indent=2, sort_keys=True).encode() + b"\n"
                info = tarfile.TarInfo("manifest.json")
                info.size, info.mode, info.mtime = len(payload), 0o644, 0
                archive.addfile(info, io.BytesIO(payload))
                for item in manifest["files"]:
                    info = tarfile.TarInfo(item["path"])
                    info.size, info.mode, info.mtime = item["size"], item["mode"], 0
                    with (stage / item["path"]).open("rb") as stream:
                        archive.addfile(info, stream)


def build(output, target):
    output = output.resolve()
    require(not output.exists(), "output already exists")
    require(not output.is_relative_to(ROOT), "write release archives outside the checkout")
    require(not command("git", "status", "--porcelain", "--untracked-files=normal"), "build requires a clean checkout")
    source = command("git", "rev-parse", "HEAD")
    rustc = command("rustc", "-vV")
    host = next(line[6:] for line in rustc.splitlines() if line.startswith("host: "))
    target = target or host
    require(target and all(c.isalnum() or c in "-_" for c in target), "invalid target")
    # Explicit even for native builds: Cargo's environment/configuration may
    # otherwise select another target while a stale native binary still exists.
    build_args = ["cargo", "build", "--workspace", "--all-features", "--release", "--locked", "--target", target]
    subprocess.run(build_args, cwd=ROOT, check=True)
    require(source == command("git", "rev-parse", "HEAD") and not command("git", "status", "--porcelain", "--untracked-files=normal"), "source changed during build")
    metadata = json.loads(command("cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"))
    binary_dir = Path(metadata["target_directory"]) / target / "release"
    programs = sorted({t["name"] for p in metadata["packages"] for t in p["targets"] if "bin" in t["kind"]})
    files = command("git", "ls-files", "-z").split("\0")
    with tempfile.TemporaryDirectory(prefix="hypercarry-release-") as temporary:
        stage = Path(temporary)
        for program in programs:
            add_file(stage, binary_dir / program, f"bin/{program}", 0o755)
        for name in files:
            if name in {"README.md", "CONTRIBUTING.md", "SECURITY.md", "LICENSE-MIT", "LICENSE-APACHE", "Cargo.lock"} or (
                name.startswith("docs/") and (name.endswith(".md") or name.endswith(".example.json"))
            ) or (name.startswith("deploy/mainnet/") and name.endswith((".service", ".json", ".conf"))) or name in {
                "tools/release_bundle.py", "tools/operator_files.py", "tools/prepare_aws.py", "tools/release_readiness.py"
            }:
                add_file(stage, ROOT / name, name, 0o644)
        manifest = {"schema_version": 1, "source_commit": source, "cargo_lock_sha256": digest_file(ROOT / "Cargo.lock"),
                    "target": target, "rustc": rustc, "profile": "release", "features": "workspace-all-features", "files": []}
        for path in sorted(stage.rglob("*")):
            if path.is_file():
                manifest["files"].append({"path": path.relative_to(stage).as_posix(), "size": path.stat().st_size,
                                          "mode": stat.S_IMODE(path.stat().st_mode), "sha256": digest_file(path)})
        require(source == command("git", "rev-parse", "HEAD") and not command("git", "status", "--porcelain", "--untracked-files=normal"), "source changed while staging bundle")
        write_archive(stage, manifest, output)
    checksum = digest_file(output)
    print(json.dumps({"bundle": str(output), "sha256": checksum, "source_commit": source, "target": target}))


def inspect_archive(archive):
    members, total = {}, 0
    for member in archive:
        name = safe_name(member.name)
        require(member.isfile() and name not in members, "archive contains links, special files or duplicate paths")
        require(0 <= member.size <= MAX_FILE and len(members) < 1024, "archive exceeds file/count limits")
        require(member.mode in (0o644, 0o755), "archive has unsafe permissions")
        total += member.size
        require(total <= MAX_TOTAL, "archive exceeds total size limit")
        members[name] = member
    require("manifest.json" in members and members["manifest.json"].size <= 1024 * 1024, "missing or oversized manifest")
    manifest = json.load(archive.extractfile(members["manifest.json"]))
    require(set(manifest) == {"schema_version", "source_commit", "cargo_lock_sha256", "target", "rustc", "profile", "features", "files"}, "invalid manifest fields")
    require(manifest["schema_version"] == 1 and manifest["profile"] == "release" and manifest["features"] == "workspace-all-features", "unsupported bundle schema/build")
    require(len(manifest["source_commit"]) == 40 and all(c in "0123456789abcdef" for c in manifest["source_commit"]), "invalid source identity")
    listed = set()
    for item in manifest["files"]:
        require(set(item) == {"path", "size", "mode", "sha256"}, "invalid file record")
        name = safe_name(item["path"])
        require(name != "manifest.json" and name not in listed and name in members, "duplicate or missing manifest file")
        member = members[name]
        require(member.size == item["size"] and member.mode == item["mode"], "manifest size/mode mismatch")
        require(digest_stream(archive.extractfile(member)) == item["sha256"], "manifest file digest mismatch")
        listed.add(name)
    require(listed | {"manifest.json"} == set(members), "archive contains unlisted files")
    require("Cargo.lock" in listed and digest_stream(archive.extractfile(members["Cargo.lock"])) == manifest["cargo_lock_sha256"], "lockfile identity mismatch")
    return manifest, members


def verified(bundle, expected):
    require(len(expected) == 64 and all(c in "0123456789abcdef" for c in expected), "require a trusted lowercase SHA-256")
    require(bundle.is_file() and not bundle.is_symlink(), "bundle must be a regular file")
    # Copy into a private snapshot while hashing. Holding the original descriptor
    # alone would not prevent an in-place mutation after checksum verification.
    stream = tempfile.TemporaryFile()
    try:
        digest, total = hashlib.sha256(), 0
        with bundle.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                total += len(chunk)
                require(total <= MAX_TOTAL, "compressed archive exceeds size limit")
                digest.update(chunk)
                stream.write(chunk)
        require(digest.hexdigest() == expected, "bundle checksum mismatch")
        stream.seek(0)
        archive = tarfile.open(fileobj=stream, mode="r:gz")
        manifest, members = inspect_archive(archive)
        return stream, archive, manifest, members
    except BaseException:
        stream.close()
        raise


def stage_bundle(bundle, expected, destination):
    require(not destination.exists() and not destination.is_symlink(), "staging destination already exists")
    stream, archive, manifest, members = verified(bundle, expected)
    try:
        destination.mkdir(mode=0o700)
        try:
            for name, member in members.items():
                path = destination / name
                path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                with path.open("xb") as output:
                    shutil.copyfileobj(archive.extractfile(member), output, 1024 * 1024)
                path.chmod(member.mode)
        except BaseException:
            shutil.rmtree(destination)
            raise
    finally:
        archive.close()
        stream.close()
    return manifest


def create_directory(path, mode):
    if path.exists():
        return
    create_directory(path.parent, 0o755)
    path.mkdir(mode=mode)
    path.chmod(mode)  # Installation permissions must not depend on root's umask.


def validate_ancestors(target, root, actual_host):
    for ancestor in target.parents:
        require(not ancestor.is_symlink(), "installation ancestor is a symlink")
        if actual_host and ancestor.exists():
            meta = ancestor.stat()
            # The nested monitoring directory intentionally has a service-owned
            # private parent; its owner is checked by the runtime directory loop.
            private_parent = ancestor == root / "var/lib/hypercarry-canary"
            require(meta.st_uid == 0 or private_parent, "installation ancestor is not root-owned")
            require(stat.S_IMODE(meta.st_mode) & 0o022 == 0, "installation ancestor is group/world writable")
        if ancestor == root:
            break


def install(bundle, expected, root, role):
    require(root.is_absolute() and root.resolve() == root, "installation root must be canonical and absolute")
    actual_host = root == Path("/")
    if actual_host:
        require(platform.system() == "Linux" and os.geteuid() == 0, "live installation requires root on Linux")
        import pwd
        account = pwd.getpwnam("hypercarry-canary")  # Explicit prerequisite; never create a login.
        owner = (account.pw_uid, account.pw_gid)
    else:
        # An alternate root stages an installation for inspection/tests only.
        owner = (os.getuid(), os.getgid())
    require(root.is_dir(), "installation root must exist")
    with tempfile.TemporaryDirectory(prefix="hypercarry-install-") as temporary:
        stage = Path(temporary) / "verified"
        manifest = stage_bundle(bundle, expected, stage)
        files = {item["path"]: item for item in manifest["files"]}
        for program in REQUIRED_PROGRAMS:
            require(files.get(f"bin/{program}", {}).get("mode") == 0o755,
                    f"bundle is missing required executable: {program}")
        for unit in ROLE_UNITS[role]:
            require(f"deploy/mainnet/{unit}" in files, f"bundle is missing required unit: {unit}")
        if actual_host:
            arch = {"x86_64": "x86_64", "aarch64": "aarch64"}.get(platform.machine())
            require(arch and manifest["target"] == f"{arch}-unknown-linux-gnu", "bundle does not match native Linux GNU target")
        copies = []
        for item in manifest["files"]:
            name = item["path"]
            if name.startswith("bin/"):
                copies.append((stage / name, root / "opt/hypercarry" / name, item["mode"]))
            elif name in {f"deploy/mainnet/{unit}" for unit in ROLE_UNITS[role]}:
                copies.append((stage / name, root / "etc/systemd/system" / Path(name).name, 0o644))
            else:
                copies.append((stage / name, root / "usr/share/doc/hypercarry" / name, 0o644))
        copies.append((stage / "manifest.json", root / "usr/share/doc/hypercarry/manifest.json", 0o644))
        # Preflight every destination before changing anything. Do not upgrade or
        # overwrite existing authority, binaries or customized service units.
        for source, target, mode in copies:
            require(not target.is_symlink(), "installation destination is a symlink")
            require(not target.exists() or (target.is_file() and digest_file(target) == digest_file(source)), f"existing file differs: {target}; stage a separately reviewed upgrade")
            if target.exists():
                require(stat.S_IMODE(target.stat().st_mode) == mode, "existing artifact permissions differ")
                require(not actual_host or target.stat().st_uid == 0, "installed artifact is not root-owned")
            validate_ancestors(target, root, actual_host)
        private = [root / "etc/hypercarry-canary", root / "var/lib/hypercarry-canary", root / "var/lib/hypercarry-canary/monitoring"]
        for directory in private:
            validate_ancestors(directory, root, actual_host)
            if directory.exists():
                require(directory.is_dir() and not directory.is_symlink() and stat.S_IMODE(directory.stat().st_mode) == 0o700, "existing runtime directory is not private")
                require(directory.stat().st_uid == owner[0], "existing runtime directory has wrong owner")
        for directory in private:
            create_directory(directory, 0o700)
            if actual_host:
                os.chown(directory, *owner)
        for source, target, mode in copies:
            create_directory(target.parent, 0o755)
            if not target.exists():
                with target.open("xb") as output, source.open("rb") as data:
                    shutil.copyfileobj(data, output, 1024 * 1024)
                target.chmod(mode)
    print(json.dumps({"installed": True, "root": str(root), "role": role, "source_commit": manifest["source_commit"],
                      "services_started": False, "configuration_created": False, "next": "Prepare reviewed configuration and run offline diagnostics; no unit was enabled."}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    builder = sub.add_parser("build")
    builder.add_argument("--output", required=True, type=Path)
    builder.add_argument("--target")
    for name in ("verify", "stage", "install"):
        item = sub.add_parser(name)
        item.add_argument("--bundle", required=True, type=Path)
        item.add_argument("--sha256", required=True)
        if name == "stage":
            item.add_argument("--destination", required=True, type=Path)
        if name == "install":
            item.add_argument("--root", type=Path, default=Path("/"))
            item.add_argument("--role", choices=ROLE_UNITS, required=True)
    args = parser.parse_args()
    if args.command == "build":
        build(args.output, args.target)
    elif args.command == "verify":
        stream, archive, manifest, _ = verified(args.bundle, args.sha256)
        archive.close()
        stream.close()
        print(json.dumps({"verified": True, "source_commit": manifest["source_commit"], "target": manifest["target"]}))
    elif args.command == "stage":
        manifest = stage_bundle(args.bundle, args.sha256, args.destination)
        print(json.dumps({"staged": str(args.destination), "source_commit": manifest["source_commit"]}))
    else:
        install(args.bundle, args.sha256, args.root, args.role)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError, tarfile.TarError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"release bundle: {error}") from error
