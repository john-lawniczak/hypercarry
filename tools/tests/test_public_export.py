import importlib.util
from pathlib import Path
import tempfile
import subprocess
import tarfile
import io
import unittest

SPEC = importlib.util.spec_from_file_location("public_export", Path(__file__).resolve().parents[1] / "check_public_export.py")
export = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(export)


class PublicExportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def write(self, name, content):
        file = self.root / name
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(content)

    def test_relative_links_and_exclusions(self):
        self.write("README.md", "[guide](docs/guide.md#section) [remote](https://example.com) [dir](docs)")
        self.write("docs/guide.md", "[home](../README.md)")
        names = ["README.md", "docs/guide.md"]
        self.assertEqual(export.check(self.root, names), [])
        self.write("docs/guide.md", "[private](../TODO.md)")
        self.assertIn("missing exported link", export.check(self.root, names)[0])
        self.assertTrue(export.excluded("Z-Explainer.md"))
        self.assertFalse(export.excluded("docs/guide.md"))

    def test_credentials_are_rejected_without_disclosure(self):
        for credential in ["AKIA" + "A" * 16, "ghp_" + "x" * 36, "-----BEGIN " + "PRIVATE KEY-----"]:
            self.write("fixture.txt", credential)
            errors = export.check(self.root, ["fixture.txt"])
            self.assertEqual(len(errors), 1)
            self.assertNotIn(credential, errors[0])
        self.write("fixture.txt", "0x" + "1" * 64)  # Public signing fixture; no raw-hex heuristic.
        self.assertEqual(export.check(self.root, ["fixture.txt"]), [])

    def test_local_artifacts_and_links_are_rejected(self):
        for name in [".env.production", ".aws/credentials", "target/binary", "data/events.jsonl", "signer.pem", "TODO.md"]:
            self.assertTrue(export.check(self.root, [name]))
        self.assertFalse(export.forbidden(".env.example"))
        self.write("good.txt", "public")
        (self.root / "alias").symlink_to(self.root / "good.txt")
        self.assertIn("symbolic links", export.check(self.root, ["alias"])[0])
        self.write("README.md", "[bad](../outside.md)")
        self.assertIn("escapes export", export.check(self.root, ["README.md"])[0])

    def test_every_z_prefix_case_extension_and_depth_is_private(self):
        for name in ["Z-notes.md", "z-trades.json", "docs/Z-secret.txt", "notes/z-live/orders.csv", "private/ledger.json", "snapshots/data/events.json"]:
            self.assertTrue(export.excluded(name), name)
            self.write(name, "private fixture")
            self.assertIn("forbidden export path", export.check(self.root, [name])[0])
        self.assertFalse(export.excluded("docs/zero-risk.md"))

    def test_pruning_is_recursive_and_does_not_follow_symlinks_or_touch_git(self):
        self.write("docs/z-trades.json", "private fixture")
        self.write("nested/Z-folder/ledger.csv", "private fixture")
        self.write("docs/guide.md", "public")
        self.write(".git/Z-config", "keep")
        outside = Path(self.temp.name).parent / (self.root.name + "-outside")
        outside.mkdir()
        self.addCleanup(lambda: outside.rmdir())
        (self.root / "z-link").symlink_to(outside, target_is_directory=True)
        export.prune_excluded(self.root)
        self.assertFalse((self.root / "docs/z-trades.json").exists())
        self.assertFalse((self.root / "nested/Z-folder").exists())
        self.assertFalse((self.root / "z-link").is_symlink())
        self.assertTrue(outside.is_dir())
        self.assertEqual((self.root / ".git/Z-config").read_text(), "keep")
        self.assertEqual((self.root / "docs/guide.md").read_text(), "public")

    def test_gitignore_and_archive_block_nested_private_material(self):
        source = Path(__file__).resolve().parents[2]
        for name in [".gitignore", ".gitattributes"]:
            self.write(name, (source / name).read_text())
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        for name in ["Z-root.json", "docs/z-notes.txt", "nested/Z-dir/ledger.csv", "data/ledger.json", "nested/DATA/ledger.json", "nested/Keystores/wallet.json"]:
            self.write(name, "private fixture")
            ignored = subprocess.run(["git", "-C", str(self.root), "check-ignore", "--quiet", name])
            self.assertEqual(ignored.returncode, 0, name)
        self.write("README.md", "public")
        subprocess.run(["git", "-C", str(self.root), "add", "-f", "."], check=True)
        subprocess.run(["git", "-C", str(self.root), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture"], check=True)
        archive = subprocess.check_output(["git", "-C", str(self.root), "archive", "HEAD"])
        with tarfile.open(fileobj=io.BytesIO(archive)) as stream:
            names = [name for name in stream.getnames() if not name.endswith("/")]
        self.assertIn("README.md", names)
        self.assertFalse(any(export.excluded(name) for name in names), names)


if __name__ == "__main__":
    unittest.main()
