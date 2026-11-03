import importlib.util
from pathlib import Path
import tempfile
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


if __name__ == "__main__":
    unittest.main()
