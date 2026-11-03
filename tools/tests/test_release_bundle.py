import importlib.util
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("release_bundle", Path(__file__).resolve().parents[1] / "release_bundle.py")
bundle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bundle)


class BundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "source"
        self.source.mkdir()
        self.manifest = {"schema_version": 1, "source_commit": "a" * 40, "cargo_lock_sha256": "", "target": "x86_64-unknown-linux-gnu",
                         "rustc": "fixture", "profile": "release", "features": "workspace-all-features", "files": []}
        for name in ["Cargo.lock", "deploy/mainnet/hypercarry-supervisor.service",
                     "deploy/mainnet/hypercarry-signer.service", "deploy/mainnet/hypercarry-watchdog.service"] + [
                         f"bin/{program}" for program in sorted(bundle.REQUIRED_PROGRAMS)]:
            path = self.source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"fixture bytes, never executed")
            mode = 0o755 if name.startswith("bin/") else 0o644
            path.chmod(mode)
            self.manifest["files"].append({"path": name, "size": path.stat().st_size, "mode": mode, "sha256": bundle.digest_file(path)})
        self.manifest["cargo_lock_sha256"] = bundle.digest_file(self.source / "Cargo.lock")
        self.archive = self.root / "release.tar.gz"
        self.rebuild()

    def rebuild(self):
        self.archive.unlink(missing_ok=True)
        bundle.write_archive(self.source, self.manifest, self.archive)
        self.checksum = bundle.digest_file(self.archive)

    def test_round_trip_and_no_overwrite(self):
        destination = self.root / "staged"
        bundle.stage_bundle(self.archive, self.checksum, destination)
        self.assertEqual((destination / "bin/hypercarry-doctor").read_bytes(), b"fixture bytes, never executed")
        with self.assertRaises(ValueError):
            bundle.stage_bundle(self.archive, self.checksum, destination)

    def test_checksum_and_manifest_changes_are_rejected(self):
        with self.assertRaises(ValueError):
            bundle.stage_bundle(self.archive, "0" * 64, self.root / "out")
        self.manifest["files"][0]["sha256"] = "0" * 64
        self.rebuild()
        with self.assertRaises(ValueError):
            bundle.stage_bundle(self.archive, self.checksum, self.root / "out")
        self.assertFalse((self.root / "out").exists())

    def test_paths_links_and_duplicates_are_rejected(self):
        for name, kind in [("../escape", tarfile.REGTYPE), ("/absolute", tarfile.REGTYPE), ("link", tarfile.SYMTYPE), ("hardlink", tarfile.LNKTYPE)]:
            with self.subTest(name=name):
                with tarfile.open(self.archive, "w:gz") as archive:
                    info = tarfile.TarInfo(name)
                    info.mode, info.type = 0o644, kind
                    archive.addfile(info, io.BytesIO(b""))
                with self.assertRaises(ValueError):
                    bundle.stage_bundle(self.archive, bundle.digest_file(self.archive), self.root / "out")
        with tarfile.open(self.archive, "w:gz") as archive:
            for _ in range(2):
                info = tarfile.TarInfo("duplicate")
                info.mode = 0o644
                archive.addfile(info, io.BytesIO(b""))
        with self.assertRaises(ValueError):
            bundle.stage_bundle(self.archive, bundle.digest_file(self.archive), self.root / "out")

    def test_install_is_role_scoped_passive_and_preserves_operator_files(self):
        stage = self.root / "installation"
        stage.mkdir()
        bundle.install(self.archive, self.checksum, stage, "executor")
        self.assertTrue((stage / "etc/systemd/system/hypercarry-supervisor.service").exists())
        self.assertFalse((stage / "etc/systemd/system/hypercarry-signer.service").exists())
        self.assertFalse((stage / "etc/systemd/system/multi-user.target.wants").exists())
        config = stage / "etc/hypercarry-canary/config.json"
        self.assertFalse(config.exists())
        config.write_text("operator state")
        bundle.install(self.archive, self.checksum, stage, "executor")
        self.assertEqual(config.read_text(), "operator state")
        (stage / "opt/hypercarry/bin/hypercarry-doctor").write_text("changed installation")
        with self.assertRaises(ValueError):
            bundle.install(self.archive, self.checksum, stage, "executor")
        self.assertEqual(config.read_text(), "operator state")

    def test_incomplete_role_cannot_report_success_or_write_installation(self):
        for role, missing in [("executor", "bin/hypercarry-supervisor"), ("recovery", "deploy/mainnet/hypercarry-watchdog.service")]:
            with self.subTest(role=role):
                original = self.manifest["files"]
                self.manifest["files"] = [item for item in original if item["path"] != missing]
                self.rebuild()
                stage = self.root / role
                stage.mkdir()
                with self.assertRaisesRegex(ValueError, "missing required"):
                    bundle.install(self.archive, self.checksum, stage, role)
                self.assertEqual(list(stage.iterdir()), [])
                self.manifest["files"] = original

    def test_verification_extracts_the_snapshot_even_if_original_is_mutated(self):
        original_open = tarfile.open
        def mutate(*args, **kwargs):
            self.archive.write_bytes(b"modified after checksum")
            return original_open(*args, **kwargs)
        with patch.object(bundle.tarfile, "open", side_effect=mutate):
            bundle.stage_bundle(self.archive, self.checksum, self.root / "snapshot")
        self.assertEqual((self.root / "snapshot/Cargo.lock").read_bytes(), b"fixture bytes, never executed")

    def test_install_rejects_symlinked_runtime_ancestor(self):
        stage = self.root / "installation"
        stage.mkdir()
        (stage / "var").symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(ValueError):
            bundle.install(self.archive, self.checksum, stage, "recovery")
        self.assertFalse((self.root / "lib/hypercarry-canary").exists())
        self.assertFalse((stage / "opt/hypercarry/bin").exists())

    def test_install_permissions_do_not_depend_on_operator_umask(self):
        stage = self.root / "installation"
        stage.mkdir()
        previous = os.umask(0o077)
        try:
            bundle.install(self.archive, self.checksum, stage, "executor")
        finally:
            os.umask(previous)
        self.assertEqual((stage / "opt/hypercarry/bin").stat().st_mode & 0o777, 0o755)
        self.assertEqual((stage / "var/lib/hypercarry-canary").stat().st_mode & 0o777, 0o700)

    def test_install_rejects_destination_symlinks(self):
        stage = self.root / "installation"
        stage.mkdir()
        (stage / "opt").symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(ValueError):
            bundle.install(self.archive, self.checksum, stage, "recovery")
        self.assertFalse((stage / "etc/hypercarry-canary").exists())


    def test_build_pins_native_target_and_never_uses_stale_default_output(self):
        repo = self.root / "checkout"
        repo.mkdir()
        (repo / "Cargo.lock").write_bytes(b"reviewed lockfile")
        target = "aarch64-apple-darwin"
        correct = repo / "target" / target / "release" / "example"
        correct.parent.mkdir(parents=True)
        correct.write_bytes(b"explicit target artifact")
        stale = repo / "target/release/example"
        stale.parent.mkdir(parents=True)
        stale.write_bytes(b"stale output must not be bundled")
        operator_files = ["tools/release_bundle.py", "tools/operator_files.py", "tools/prepare_aws.py",
                          "tools/release_readiness.py", "deploy/mainnet/aws-reference.json", "deploy/mainnet/aws-state-mount.conf"]
        for name in operator_files:
            source = repo / name
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_bytes(b"reviewed operator tooling fixture")
        dirty = False
        def command(*args):
            if args[:2] == ("git", "status"):
                return " M Cargo.lock" if dirty else ""
            if args[:2] == ("git", "rev-parse"):
                return "a" * 40
            if args == ("rustc", "-vV"):
                return "host: " + target
            if args[:2] == ("cargo", "metadata"):
                return json.dumps({"target_directory": str(repo / "target"), "packages": [{"targets": [{"name": "example", "kind": ["bin"]}]}]})
            if args[:2] == ("git", "ls-files"):
                return "\0".join(["Cargo.lock"] + operator_files)
            raise AssertionError(args)
        output = self.root / "native.tar.gz"
        with patch.object(bundle, "ROOT", repo), patch.object(bundle, "command", side_effect=command), patch.object(bundle.subprocess, "run") as run:
            with patch.dict(os.environ, {"CARGO_BUILD_TARGET": "unexpected-target"}):
                bundle.build(output, None)
            self.assertEqual(run.call_args.args[0][-2:], ["--target", target])
            bundle.stage_bundle(output, bundle.digest_file(output), self.root / "native")
            self.assertEqual((self.root / "native/bin/example").read_bytes(), b"explicit target artifact")
            for name in operator_files:
                self.assertEqual((self.root / "native" / name).read_bytes(), b"reviewed operator tooling fixture")
            original_add = bundle.add_file
            def change_source(*args):
                nonlocal dirty
                original_add(*args)
                dirty = True
            changed_output = self.root / "changed.tar.gz"
            with patch.object(bundle, "add_file", side_effect=change_source):
                with self.assertRaisesRegex(ValueError, "source changed while staging"):
                    bundle.build(changed_output, None)
            self.assertFalse(changed_output.exists())


if __name__ == "__main__":
    unittest.main()
