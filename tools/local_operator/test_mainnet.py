"""Offline mainnet gate regressions. Only public scalar-one fixture signatures."""
import copy
import runpy
from pathlib import Path
import tempfile
import unittest
import warnings
from unittest.mock import patch

from eth_account import Account
from hyperliquid.utils.signing import sign_l1_action

from .entry import artifact, execute_open, plan_open, reconcile_open
from .mainnet import MainnetVenue
from .release import PROGRAMS, SCENARIOS, Release, candidate, verify_candidate
from .signer import Session, now_ms, wire_action
from .store import digest, read, write
from . import test_entry
from .test_entry import ANALYTICS, JOURNAL, EntryVenue
from .test_signer import bracket, policy
from .test_workflow import ACCOUNT, AGENT
from .venue import Venue
from .workflow import execute_close, plan_close, reconcile, seal_plan


class MainnetTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.wallet = Account.from_key(bytes.fromhex("00" * 31 + "01"))
        write(self.root / "evidence.json", {"fixture": "offline only; no release attestation"})
        item = artifact(self.root / "evidence.json")
        self.candidate = {"binaries": {k: item for k in PROGRAMS}, "fixture": True}
        self.plan = {"operation": "open", "policy": policy(), "action": bracket(),
                     "tools": {k: item for k in ("analytics", "journal")},
                     "calculated": {"input": {"notional_budget": "20"}},
                     "candidate_sha256": digest(self.candidate)}
        self.plan["policy"].update(network="mainnet", expires_at_ms=now_ms()+120000,
                                   signer_address=self.wallet.address.lower(), action_ttl_ms=30000)
        seal_plan(self.plan)
        self.file = self.root / "release.json"
        self.value = {"schema_version": 1, "kind": "attended_mainnet_bracket_release",
            "candidate": self.candidate, "plan_digest": digest(self.plan), "account": ACCOUNT,
            "agent": self.wallet.address.lower(), "testnet_agent": AGENT,
            "operations_dir": str(self.root / "ops"), "signer_dir": str(self.root / "signer"),
            "stop_file": str(self.root / "STOP"), "expires_at_ms": now_ms()+180000,
            "decision": "approve", "operator": "fixture owner", "reviewer": "fixture reviewer",
            "checks": {"offline_tests": True, "dependency_policy": True, "advisory_audit": True},
            "evidence": [{"schema_version": 1, "candidate_sha256": digest(self.candidate), "scenario": scenario,
                "outcome": "passed", "operator": "fixture owner", "reviewer": "fixture reviewer",
                "performed_at_ms": now_ms()-1, "observations": "Offline fixture, never qualifying evidence",
                "artifacts": [{"path": "evidence.json", "sha256": item["sha256"]}],
                "session_id": scenario if scenario.endswith("lifecycle") or scenario == "fault_restart" else None}
                for scenario in sorted(SCENARIOS)]}
        verifier = patch("tools.local_operator.release.verify_candidate")
        self.verify = verifier.start()
        self.addCleanup(verifier.stop)

    def release(self):
        write(self.file, self.value, replace=True)
        return Release(self.file, digest(self.value), self.plan, self.root, enabled=True)

    def signed(self, *, mainnet=True):
        nonce = now_ms()
        action = wire_action(self.plan["action"])
        expires = nonce + 30000
        return {"action": action, "nonce": nonce, "expiresAfter": expires, "vaultAddress": None,
                "signature": sign_l1_action(self.wallet, action, None, nonce, expires, mainnet)}

    def test_default_paths_cannot_enable_mainnet(self):
        with patch("tools.local_operator.mainnet.main") as entrypoint, warnings.catch_warnings():
            warnings.simplefilter("ignore", RuntimeWarning)
            runpy.run_module("tools.local_operator.mainnet", run_name="__main__")
            entrypoint.assert_called_once_with()
        with self.assertRaises(ValueError):
            Release(self.file, "ab"*32, self.plan, self.root)
        with self.assertRaises(ValueError):
            Session(self.plan["policy"], self.root / "signer")
        with patch.object(Venue, "_post") as post:
            for call in (lambda: Venue("mainnet").exchange({}), lambda: Venue("mainnet").post("exchange", {})):
                with self.assertRaises(ValueError): call()
            post.assert_not_called()

    def test_release_rejects_incomplete_or_mismatched_evidence(self):
        original = copy.deepcopy(self.value)
        changes = [lambda v: v["evidence"].pop(),
            lambda v: v["evidence"][0].update(candidate_sha256="00"*32),
            lambda v: v["evidence"][0].update(reviewer="fixture OWNER"),
            lambda v: v.update(reviewer="fixture owner"),
            lambda v: v.update(testnet_agent=v["agent"]),
            lambda v: v.update(expires_at_ms=now_ms()-1),
            lambda v: v.update(plan_digest="ab"*32),
            lambda v: v["checks"].update(advisory_audit=False),
            lambda v: v["evidence"][0]["artifacts"][0].update(path="../evidence.json"),
            lambda v: v.update(signer_dir=v["operations_dir"]),
            lambda v: v.update(stop_file=str(self.root)+"/missing/../STOP")]
        for change in changes:
            self.value = copy.deepcopy(original)
            change(self.value)
            with self.subTest(change=changes.index(change)), self.assertRaises(ValueError): self.release()
        self.value = copy.deepcopy(original)
        sessions = [r for r in self.value["evidence"] if r["session_id"]]
        sessions[0]["session_id"] = sessions[1]["session_id"]
        with self.assertRaisesRegex(ValueError, "distinct"): self.release()

    def test_release_rechecks_source_plan_artifacts_stop_and_directories(self):
        release = self.release()
        with patch("tools.local_operator.signer.now_ms", return_value=1801440000000):
            with self.assertRaisesRegex(ValueError, "maintenance exception expired"): release.check()
        self.verify.side_effect = ValueError("source changed")
        with self.assertRaisesRegex(ValueError, "source changed"): release.check()
        self.verify.side_effect = None
        with self.assertRaisesRegex(ValueError, "directory"):
            release.authorize(self.plan["policy"], operations=self.root / "elsewhere")
        with self.assertRaisesRegex(ValueError, "directory"):
            Session(self.plan["policy"], self.root / "elsewhere", release=release)
        self.plan["action"]["orders"][0]["s"] = "1"
        with self.assertRaises(ValueError): release.check()
        self.plan["action"]["orders"][0]["s"] = "0.1"
        stop = Path(self.value["stop_file"])
        stop.touch()
        with self.assertRaisesRegex(ValueError, "STOP"): release.check()
        stop.unlink()
        write(self.root / "evidence.json", {"changed": True}, replace=True)
        with self.assertRaisesRegex(ValueError, "artifact changed"): release.check()

    def test_signer_reserves_before_signature_and_rejects_replay_or_network_drift(self):
        release = self.release()
        p = self.plan["policy"]
        session = Session(p, self.value["signer_dir"], release=release)
        now = now_ms()
        request = {"schema_version": 1, "network": "mainnet", "key_id": p["key_id"],
                   "signer_address": p["signer_address"], "nonce": now, "expires_after": now+10000,
                   "action": self.plan["action"]}
        wrong = {**request, "network": "testnet"}
        with self.assertRaises(ValueError): session.reserve(wrong, now)
        session.reserve(request, now)
        self.assertTrue(read(session.path)["entry_signed"])
        with self.assertRaisesRegex(ValueError, "nonce replay"):
            Session(p, self.value["signer_dir"], release=release).reserve(request, now)
        changed = copy.deepcopy(request)
        changed["action"]["orders"][0]["c"] = "0x"+"ff"*16
        with self.assertRaisesRegex(ValueError, "exact action"): session.reserve(changed, now)

    def test_mainnet_domain_and_one_attempt_after_lost_ack(self):
        venue = MainnetVenue(self.release())
        with patch("tools.local_operator.mainnet.fresh_action_evidence"), \
             patch("tools.local_operator.mainnet.final_entry_evidence"), \
             patch("tools.local_operator.mainnet.verify_agent"), \
             patch.object(venue, "_post", side_effect=TimeoutError("fixture lost ack")) as post:
            with self.assertRaisesRegex(ValueError, "domain/agent"): venue.exchange(self.signed(mainnet=False))
            post.assert_not_called()
            signed = self.signed()
            with self.assertRaises(TimeoutError): venue.exchange(signed)
            with self.assertRaises(FileExistsError): venue.exchange(signed)
            self.assertEqual(post.call_count, 1)
        records = list((self.root / "ops/mainnet-wire-reservations").glob("*.json"))
        self.assertEqual(len(records), 2)
        self.assertTrue(all("signature" not in r.read_text() for r in records))

    def test_last_stop_or_endpoint_check_prevents_broadcast(self):
        venue = MainnetVenue(self.release())
        with patch("tools.local_operator.mainnet.fresh_action_evidence"), \
             patch("tools.local_operator.mainnet.final_entry_evidence"), \
             patch("tools.local_operator.mainnet.verify_agent", side_effect=lambda *_: Path(self.value["stop_file"]).touch()), \
             patch.object(venue, "_post") as post:
            with self.assertRaisesRegex(ValueError, "STOP"): venue.exchange(self.signed())
            post.assert_not_called()
        Path(self.value["stop_file"]).unlink()
        venue.base = "https://api.hyperliquid-testnet.xyz/"
        with self.assertRaisesRegex(ValueError, "endpoint"): venue.exchange(self.signed())

    def test_mainnet_reconciliation_cannot_use_testnet_observations(self):
        for run in (lambda: reconcile(Venue("testnet"), self.plan),
                    lambda: reconcile_open(Venue("testnet"), self.plan, self.root / "ops")):
            with self.assertRaisesRegex(ValueError, "network mismatch"): run()

    def test_pilot_limits_are_checked_independently_of_plan_hash(self):
        original = copy.deepcopy(self.plan)
        changes = [lambda p: p["calculated"]["input"].update(notional_budget="21"),
                   lambda p: p["policy"].update(quantity="1"),
                   lambda p: p["policy"].update(coin="xyz:BTC"),
                   lambda p: p["policy"].update(leverage=2),
                   lambda p: p.update(operation="protect")]
        for change in changes:
            self.plan = copy.deepcopy(original)
            change(self.plan)
            self.bind_plan()
            with self.subTest(change=changes.index(change)), self.assertRaises(ValueError): self.release()

    @unittest.skipUnless(ANALYTICS.exists() and JOURNAL.exists(), "build Rust integration binaries")
    def test_real_mainnet_journal_full_lifecycle_and_owned_close(self):
        self.lifecycle()

    @unittest.skipUnless(ANALYTICS.exists() and JOURNAL.exists(), "build Rust integration binaries")
    def test_real_mainnet_journal_partial_fill_repair_and_owned_close(self):
        self.lifecycle(partial=True)

    @unittest.skipUnless(ANALYTICS.exists() and JOURNAL.exists(), "build Rust integration binaries")
    def test_uncertain_entry_reconciliation_retains_ownership_for_later_close(self):
        self.lifecycle(unknown=True)

    def lifecycle(self, partial=False, unknown=False):
        fake = EntryVenue()
        fake.network = "mainnet"
        inputs = {"network": "mainnet", "account": ACCOUNT, "coin": "BTC", "direction": "long",
                  "notional_budget": "19.1", "leverage": 1, "reserve_usdc": "400",
                  "entry_slippage_percent": "0.5", "exit_slippage_percent": "10",
                  "take_profit": "120", "stop_loss": "90", "min_health_at_stop": "2"}
        self.plan = plan_open(fake, inputs, self.wallet.address.lower(), "fixture", ANALYTICS, JOURNAL, mainnet_preview=True)
        self.candidate["binaries"].update(self.plan["tools"])
        self.bind_plan()
        venue = MainnetVenue(self.release())
        venue.info = fake.info
        submit = test_entry.EntryTests.submitter(None, partial=partial, unknown=unknown)
        def entry_send(_venue, *args, **kwargs):
            p, action, _socket, attempt, label = args
            write(attempt / (label + "-intent.json"), {"schema_version": 1, "network": p["network"],
                  "key_id": p["key_id"], "signer_address": p["signer_address"],
                  "action": action, "nonce": now_ms(), "expires_after": p["expires_at_ms"]})
            return submit(fake, *args, **kwargs)
        with patch("tools.local_operator.entry.send", side_effect=entry_send):
            result = execute_open(venue, self.plan, "unused", self.value["operations_dir"])
        if unknown:
            self.assertEqual(result["status"], "reconciliation_required")
            # A later independent fixture observation resolves the existing IDs.
            # No additional exchange call is made during reconciliation.
            fake.order_status = "filled"
            test_entry.EntryTests.submitter(None)(fake, self.plan["policy"], self.plan["action"], "unused", self.root, "entry")
            with patch.object(venue, "_post") as post:
                result = reconcile_open(venue, self.plan, self.value["operations_dir"])
                post.assert_not_called()
        self.assertEqual(result["status"], "open")
        entry_session = self.plan["policy"]["session_id"]
        owned = read(self.root / "ops" / entry_session / "position.json")
        self.assertEqual(owned["network"], "mainnet")
        self.plan = plan_close(fake, owned, self.wallet.address.lower(), "fixture", mainnet_preview=True)
        self.plan["pilot_entry_session"] = entry_session
        self.bind_plan()
        release = self.release()
        venue = MainnetVenue(release)
        venue.info = fake.info
        def close_send(_venue, _policy, action, _socket, _attempt, _label, **callbacks):
            release.authorize_action(action)
            if callbacks.get("before_submit"): callbacks["before_submit"]()
            if action["type"] == "order": fake.position = None
            else: fake.orders = []
            return {"status": "ok"}
        with patch("tools.local_operator.workflow.send", side_effect=close_send):
            result = execute_close(venue, self.plan, "unused", self.value["operations_dir"])
        self.assertEqual(result["status"], "closed")
        self.assertFalse((self.root / "ops" / ("mainnet-"+ACCOUNT+"-pending.json")).exists())

    def bind_plan(self):
        self.plan["candidate_sha256"] = digest(self.candidate)
        seal_plan(self.plan)
        self.value["plan_digest"] = digest(self.plan)
        self.value["expires_at_ms"] = self.plan["policy"]["expires_at_ms"] + 1000
        for record in self.value["evidence"]: record["candidate_sha256"] = digest(self.candidate)


class CandidateTests(unittest.TestCase):
    def test_identity_binds_clean_source_lock_sdk_and_each_binary(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / "Cargo.lock").write_text("fixture lock")
            binaries = {}
            for key, program in PROGRAMS.items():
                binaries[key] = root / program
                binaries[key].write_text(program)
            with patch("tools.local_operator.release.subprocess.check_output", side_effect=lambda args, **_: "" if "status" in args else "ab"*20), \
                 patch("tools.local_operator.release.importlib.metadata.version", return_value="0.24.0"):
                value = candidate(root, binaries)
                write(root / "candidate.json", value)
                value = read(root / "candidate.json")
                verify_candidate(value, root)
                binaries["recovery"].write_text("changed")
                with self.assertRaisesRegex(ValueError, "changed"): verify_candidate(value, root)
            with patch("tools.local_operator.release.subprocess.check_output", return_value=" M source.py"):
                with self.assertRaisesRegex(ValueError, "clean"): candidate(root, binaries)
