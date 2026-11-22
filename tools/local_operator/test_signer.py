import copy
from pathlib import Path
import tempfile
import unittest

from .signer import Session, validate_policy
from .store import read


def policy():
    return {"schema_version": 1, "network": "testnet", "account": "0x" + "11" * 20,
            "signer_address": "0x" + "22" * 20, "key_id": "fixture-agent", "session_id": "fixture",
            "expires_at_ms": 60000, "asset": 0, "coin": "BTC", "is_buy": False, "quantity": "0.1",
            "entry_limit": "99", "tp": "80", "sl": "110", "tp_limit": "88", "sl_limit": "121",
            "close_min": "70", "close_max": "125", "leverage": 1, "allow_entry": True,
            "cancel_oids": [123, 456], "max_actions": 10, "plan_digest": "ab" * 32, "reserve_usdc": "0"}


def order(identifier, *, reduce=True, kind=None, size="0.1", price="101"):
    return {"a": 0, "b": reduce, "r": reduce, "s": size, "p": price,
            "t": kind or {"limit": {"tif": "Ioc"}}, "c": "0x" + f"{identifier:032x}"}


def bracket():
    return {"type": "order", "grouping": "normalTpsl", "orders": [
        order(1, reduce=False, price="99"),
        order(2, price="88", kind={"trigger": {"isMarket": True, "triggerPx": "80", "tpsl": "tp"}}),
        order(3, price="121", kind={"trigger": {"isMarket": True, "triggerPx": "110", "tpsl": "sl"}})]}


def request(action, nonce=1000):
    return {"schema_version": 1, "network": "testnet", "key_id": "fixture-agent",
            "signer_address": "0x" + "22" * 20, "nonce": nonce, "expires_after": nonce + 5000,
            "action": action}


class SignerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name).resolve()
        self.session = Session(policy(), self.path, 1000)

    def test_restart_cannot_sign_entry_again_even_with_new_ids(self):
        self.session.reserve(request(bracket()), 1000)
        again = bracket()
        for i, item in enumerate(again["orders"]):
            item["c"] = "0x" + f"{i+20:032x}"
        with self.assertRaises(ValueError):
            Session(policy(), self.path, 1001).reserve(request(again, 1001), 1001)
        self.assertTrue(read(self.session.path)["entry_signed"])

    def test_rejected_actions_do_not_consume_budget(self):
        for change in (lambda a: a.update(type="withdraw3"),
                       lambda a: a["orders"][0].update(s="0.2"),
                       lambda a: a["orders"][0].update(a=1),
                       lambda a: a.update(builder={"b": "unapproved"}),
                       lambda a: a["orders"][1].update(r=False),
                       lambda a: a["orders"][2]["t"]["trigger"].update(triggerPx="150")):
            candidate = bracket()
            change(candidate)
            with self.assertRaises(ValueError):
                self.session.reserve(request(candidate), 1000)
        self.assertFalse(self.session.path.exists())

    def test_nonce_domain_shared_across_sessions(self):
        self.session.reserve(request(bracket()), 1000)
        p = policy()
        p["session_id"] = "second"
        with self.assertRaises(ValueError):
            Session(p, self.path, 1000).reserve(request(bracket()), 1000)

    def test_identity_expiry_and_policy_drift(self):
        for key, value in (("network", "mainnet"), ("key_id", "other"),
                           ("expires_after", 999), ("nonce", 1001)):
            candidate = request(bracket())
            candidate[key] = value
            with self.assertRaises(ValueError):
                self.session.reserve(candidate, 1000)
        self.session.reserve(request(bracket()), 1000)
        p = policy()
        p["quantity"] = "1"
        with self.assertRaises(ValueError):
            Session(p, self.path, 1001).reserve(request(bracket(), 1001), 1001)

    def test_longer_action_lifetime_requires_explicit_bounded_policy(self):
        candidate = request(bracket())
        candidate["expires_after"] = 31000
        with self.assertRaises(ValueError):
            self.session.reserve(candidate, 1000)
        p = policy()
        p["action_ttl_ms"] = 30000
        Session(p, self.path, 1000).reserve(candidate, 1000)
        for invalid in (True, 9999, 30001, "30000"):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                validate_policy({**p, "action_ttl_ms": invalid}, 1000)

    def test_action_lifetime_cannot_extend_session_expiry(self):
        p = {**policy(), "action_ttl_ms": 30000, "expires_at_ms": 20000}
        candidate = request(bracket())
        candidate["expires_after"] = 20001
        with self.assertRaises(ValueError):
            Session(p, self.path, 1000).reserve(candidate, 1000)

    def test_close_is_reduce_only_cumulative_and_bounded(self):
        p = policy()
        p["allow_entry"] = False
        session = Session(p, self.path, 1000)
        action = {"type": "order", "grouping": "na", "orders": [order(1, size="0.06")]}
        session.reserve(request(action), 1000)
        action["orders"][0]["c"] = "0x" + "ab" * 16
        with self.assertRaises(ValueError):
            session.reserve(request(action, 1001), 1001)
        action["orders"][0]["s"] = "0.04"
        session.reserve(request(action, 1001), 1001)
        self.assertEqual(read(session.path)["close_quantity"], "0.10")

    def test_only_explicit_owned_cancels(self):
        bad = {"type": "cancel", "cancels": [{"a": 0, "o": 999}]}
        with self.assertRaises(ValueError):
            self.session.reserve(request(bad), 1000)
        good = {"type": "cancel", "cancels": [{"a": 0, "o": 123}]}
        self.session.reserve(request(good), 1000)
        with self.assertRaises(ValueError):
            self.session.reserve(request(good, 1001), 1001)

    def test_recovery_cancel_ids_are_exact_and_do_not_authorize_entries_or_repairs(self):
        p = policy()
        p.update(allow_entry=False, cancel_oids=[], recovery_cloids=["0x" + f"{i:032x}" for i in (1, 2, 3)])
        session = Session(p, self.path, 1000)
        action = {"type": "cancelByCloid", "cancels": [{"asset": 0, "cloid": p["recovery_cloids"][1]}]}
        with self.assertRaises(ValueError): self.session.reserve(request(action), 1000)
        session.reserve(request(action), 1000)
        with self.assertRaises(ValueError): session.reserve(request(action, 1001), 1001)
        with self.assertRaises(ValueError): session.reserve(request(bracket(), 1001), 1001)
        repair = {"type": "order", "grouping": "na", "orders": [bracket()["orders"][2]]}
        with self.assertRaises(ValueError): session.reserve(request(repair, 1001), 1001)

    def test_no_owner_key_or_mainnet_policy(self):
        for key, value in (("network", "mainnet"), ("signer_address", policy()["account"]),
                           ("expires_at_ms", 4_000_000), ("quantity", "NaN")):
            candidate = copy.deepcopy(policy())
            candidate[key] = value
            with self.assertRaises(ValueError):
                validate_policy(candidate, 1000)

    def test_symlink_record_is_rejected(self):
        target = self.path / "target"
        target.write_text("{}")
        self.session.path.symlink_to(target)
        with self.assertRaises(OSError):
            self.session.reserve(request(bracket()), 1000)
