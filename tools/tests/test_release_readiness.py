import json
from pathlib import Path
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import release_readiness as readiness
import test_release_bundle as fixtures


class ReadinessTests(unittest.TestCase):
    def setUp(self):
        self.bundle = fixtures.BundleTests()
        self.bundle.setUp()
        self.addCleanup(self.bundle.doCleanups)
        self.root = self.bundle.root
        self.configs = {}
        for name in readiness.CONFIGS:
            path = self.root / (name + ".json")
            path.write_text(json.dumps({"fixture": name}))
            self.configs[name] = path
        self.candidate = self.root / "candidate"
        self.result = readiness.prepare(self.bundle.archive, self.bundle.checksum, self.configs, self.candidate)
        self.digest = self.result["candidate_sha256"]

    def record(self, scenario, /, **changes):
        directory = self.root / scenario
        directory.mkdir(exist_ok=True)
        artifact = directory / "observation.txt"
        artifact.write_bytes(("SYNTHETIC TEST ONLY " + scenario).encode())
        record = {"schema_version": 1, "candidate_sha256": self.digest, "scenario": scenario, "outcome": "passed",
                  "operator": "fixture-operator", "reviewer": "fixture-reviewer", "performed_at": "2026-01-01T00:00:00Z",
                  "observations": "Synthetic claims exercise validation; no operational exercise occurred.",
                  "session_id": scenario if scenario.startswith("testnet_session_") else None,
                  "artifacts": [{"path": artifact.name, "sha256": readiness.sha256(artifact.read_bytes())}]}
        record.update(changes)
        path = directory / "record.json"
        path.write_text(json.dumps(record))
        return path

    def verify(self, records=None):
        return readiness.verify(self.candidate, self.digest, self.configs, records or {})

    def test_pending_then_complete_integrity_report_never_authorizes_trading(self):
        pending = self.verify()
        self.assertFalse(pending["evidence_complete_for_review"])
        self.assertEqual(set(pending["missing"]), set(readiness.SCENARIOS))
        records = {scenario: self.record(scenario) for scenario in readiness.SCENARIOS}
        complete = self.verify(records)
        self.assertTrue(complete["evidence_complete_for_review"])
        self.assertFalse(complete["trading_authorized"])
        records["alert_delivery"] = self.record("alert_delivery", outcome="failed")
        self.assertEqual(self.verify(records)["failed"], ["alert_delivery"])
        del records["security_review"]
        self.assertIn("security_review", self.verify(records)["missing"])

    def test_manifest_bundle_and_current_config_mutation_are_rejected(self):
        targets = [self.candidate / "candidate.json", self.candidate / "release.tar.gz",
                   self.candidate / "config/runtime.json", self.configs["runtime"]]
        for path in targets:
            with self.subTest(path=path):
                original = path.read_bytes()
                path.write_bytes(original + b" ")
                with self.assertRaises(ValueError):
                    self.verify()
                path.write_bytes(original)

    def test_identity_independence_future_evidence_and_session_reuse_are_rejected(self):
        for changes in [{"candidate_sha256": "0" * 64}, {"scenario": "other"}, {"reviewer": " FIXTURE-OPERATOR "},
                        {"performed_at": "2999-01-01T00:00:00Z"}, {"artifacts": []}, {"outcome": "pending"}]:
            with self.subTest(changes=changes):
                with self.assertRaises(ValueError):
                    self.verify({"security_review": self.record("security_review", **changes)})
        records = {name: self.record(name, session_id="one-session") for name in ("testnet_session_1", "testnet_session_2")}
        with self.assertRaisesRegex(ValueError, "distinct identities"):
            self.verify(records)

    def test_artifact_drift_traversal_and_symlinks_are_rejected(self):
        record = self.record("rollback_drill")
        artifact = record.parent / "observation.txt"
        original = artifact.read_bytes()
        artifact.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "artifact digest mismatch"):
            self.verify({"rollback_drill": record})
        artifact.unlink()
        outside = self.root / "outside"
        outside.write_bytes(original)
        artifact.symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.verify({"rollback_drill": record})
        value = json.loads(record.read_text())
        value["artifacts"][0]["path"] = "../outside"
        record.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "unsafe"):
            self.verify({"rollback_drill": record})

    def test_freeze_requires_complete_linux_release_and_never_overwrites(self):
        with self.assertRaises(FileExistsError):
            readiness.prepare(self.bundle.archive, self.bundle.checksum, self.configs, self.candidate)
        self.bundle.manifest["target"] = "aarch64-apple-darwin"
        self.bundle.rebuild()
        with self.assertRaisesRegex(ValueError, "Linux"):
            readiness.prepare(self.bundle.archive, self.bundle.checksum, self.configs, self.root / "other")
        self.assertFalse((self.root / "other").exists())
        self.bundle.manifest["target"] = "x86_64-unknown-linux-gnu"
        self.bundle.manifest["files"] = [item for item in self.bundle.manifest["files"] if item["path"] != "bin/hypercarry-watchdog"]
        self.bundle.rebuild()
        with self.assertRaisesRegex(ValueError, "missing required executable"):
            readiness.prepare(self.bundle.archive, self.bundle.checksum, self.configs, self.root / "other")

    def test_named_paths_reject_unknown_duplicates_and_incomplete_configs(self):
        for values in [["unknown=/tmp/a"], ["runtime=/tmp/a", "runtime=/tmp/b"], ["runtime="]]:
            with self.assertRaises(ValueError):
                readiness.named_paths(values, readiness.CONFIGS)
        with self.assertRaises(ValueError):
            readiness.configurations({"runtime": self.configs["runtime"]})

    def test_cli_missing_evidence_exits_two_and_report_is_exclusive(self):
        report = self.root / "report.json"
        command = [sys.executable, "-B", str(Path(readiness.__file__)), "verify",
                   "--candidate", str(self.candidate), "--sha256", self.digest, "--report", str(report)]
        for name, path in self.configs.items():
            command.extend(["--config", name + "=" + str(path)])
        result = subprocess.run(command, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertEqual(json.loads(result.stdout), json.loads(report.read_bytes()))
        self.assertFalse(json.loads(result.stdout)["trading_authorized"])
        original = report.read_bytes()
        retry = subprocess.run(command, capture_output=True, text=True, timeout=10)
        self.assertNotEqual(retry.returncode, 0)
        self.assertEqual(report.read_bytes(), original)
