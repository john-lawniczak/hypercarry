import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

from .monitor import cached, watch
from .signer import now_ms
from .store import write
from .test_workflow import ACCOUNT


class MonitorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.binary = self.root / "fixture-binary"
        self.binary.write_text("public fixture")
        self.cache = self.root / "cache"

    def test_existing_rust_archive_path_and_exit_failure_are_preserved(self):
        snapshot = {"schema_version": 2, "network": "testnet", "account": ACCOUNT, "completed_at_ms": now_ms(), "dexs": []}
        result = SimpleNamespace(returncode=14, stdout=json.dumps(snapshot))
        with patch("tools.local_operator.monitor.subprocess.run", return_value=result) as run:
            value = watch(self.binary, "testnet", ACCOUNT, self.cache, once=True)
            self.assertIn("--archive-dir", run.call_args.args[0])
        self.assertEqual(value["status"], "protection_missing")
        self.assertTrue(cached(self.cache, "testnet", ACCOUNT)["usable"])

    def test_read_failure_invalidates_cache_instead_of_reusing_old_success(self):
        result = SimpleNamespace(returncode=14, stdout="")
        with patch("tools.local_operator.monitor.subprocess.run", return_value=result):
            watch(self.binary, "testnet", ACCOUNT, self.cache, once=True)
        self.assertFalse(cached(self.cache, "testnet", ACCOUNT)["usable"])

    def test_stale_wrong_account_and_shared_collector_directory(self):
        write(self.cache / "latest.json", {"network": "testnet", "account": ACCOUNT,
              "observed_at_ms": now_ms() - 60_000, "status": "protected", "snapshot": {}})
        self.assertFalse(cached(self.cache, "testnet", ACCOUNT)["usable"])
        with self.assertRaises(ValueError):
            cached(self.cache, "mainnet", ACCOUNT)
        write(self.cache / "collector-identity.json", {"network": "mainnet", "account": ACCOUNT})
        with self.assertRaises(ValueError):
            watch(self.binary, "testnet", ACCOUNT, self.cache, once=True)
