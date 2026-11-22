import json
import unittest
from unittest.mock import patch
from .discovery import bootstrap, names
from .signer import now_ms
from .test_workflow import ACCOUNT


class Socket:
    def __init__(self, missing=False, changed=False, wrong=False):
        state = {"assetPositions": [], "time": now_ms()}
        data = {"user": ACCOUNT, "clearinghouseStates": [["", state], ["i<3fl", state]]}
        self.events = [{"channel": "allDexsClearinghouseState", "data": data}]
        for dex in ("", "i<3fl"):
            self.events.append({"channel": "openOrders", "data": {"user": ACCOUNT, "dex": dex, "orders": []}})
        last = json.loads(json.dumps(data))
        if missing: last["clearinghouseStates"].pop()
        if wrong: last["user"] = "0x" + "55" * 20
        if changed:
            last["clearinghouseStates"][0][1]["assetPositions"] = [{"position": {
                "coin": "BTC", "szi": "1", "entryPx": "100", "leverage": {"type": "isolated", "value": 1}}}]
        self.events.append({"channel": "subscriptionResponse", "data": {"method": "subscribe",
            "subscription": {"type": "allDexsClearinghouseState", "user": ACCOUNT}}})
        self.events.append({"channel": "allDexsClearinghouseState", "data": last})
        self.closed = False
        self.sent = []
    def send(self, text): self.sent.append(json.loads(text))
    def recv(self): return json.dumps(self.events.pop(0))
    def close(self): self.closed = True


class DiscoveryTests(unittest.TestCase):
    def test_complete_fresh_bootstrap_and_safe_non_alphanumeric_discovery(self):
        expected = names([None, {"name": "i<3fl"}])
        s = Socket()
        result = bootstrap("testnet", ACCOUNT, expected, lambda *a, **kw: s)
        self.assertEqual([d["dex"] for d in result], expected)
        self.assertEqual(len(s.sent), 5)
        self.assertTrue(s.closed)

    def test_incomplete_wrong_account_and_changed_positions_fail_closed(self):
        for kw in ({"missing": True}, {"wrong": True}, {"changed": True}):
            s = Socket(**kw)
            with self.assertRaises(ValueError):
                bootstrap("testnet", ACCOUNT, ["", "i<3fl"], lambda *a, **k: s)
            self.assertTrue(s.closed)

    def test_missing_order_subscription_never_means_flat(self):
        s = Socket()
        s.events.pop(2)
        with self.assertRaises(IndexError):
            bootstrap("testnet", ACCOUNT, ["", "i<3fl"], lambda *a, **k: s)
        self.assertTrue(s.closed)

    def test_discovery_limit_and_duplicates(self):
        self.assertEqual(len(names([None] + [{"name": str(i)} for i in range(282)])), 283)
        for bad in ([None, {"name": "x"}, {"name": "x"}], [None] * 513):
            with self.assertRaises(ValueError): names(bad)
