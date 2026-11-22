from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from .protection import execute_protect, plan_protect
from .test_workflow import ACCOUNT, AGENT, FakeVenue
from .workflow import adoption


class ProtectionTests(unittest.TestCase):
    def setUp(self):
        self.venue = FakeVenue()
        self.venue.position["liquidationPx"] = "190"
        self.record = adoption(self.venue, ACCOUNT, "BTC")
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def plan(self):
        return plan_protect(self.venue, self.record, AGENT, "fixture", "80", "105")

    def test_old_stops_survive_rejected_replacement(self):
        plan = self.plan()
        with patch("tools.local_operator.protection.send", return_value={"status": "ok"}) as send:
            with self.assertRaises(ValueError):
                execute_protect(self.venue, plan, "unused", self.root)
            self.assertEqual(send.call_count, 1)
        self.assertEqual({o["oid"] for o in self.venue.orders}, {123, 456})

    def test_verify_new_pair_before_retiring_originals(self):
        plan = self.plan()
        def submit(venue, policy, action, *args, **kwargs):
            if kwargs.get("before_submit"):
                kwargs["before_submit"]()
            if action["type"] == "order":
                self.assertEqual({o["oid"] for o in venue.orders}, {123, 456})
                for i, order in enumerate(action["orders"]):
                    trigger = order["t"]["trigger"]
                    venue.orders.append({"coin": "BTC", "side": "B", "oid": 700+i, "cloid": order["c"],
                        "sz": order["s"], "limitPx": order["p"], "triggerPx": trigger["triggerPx"],
                        "reduceOnly": True, "isTrigger": True, "children": [],
                        "orderType": "Take Profit Market" if i == 0 else "Stop Market"})
            else:
                self.assertEqual(len(venue.orders), 4)
                old = {o["o"] for o in action["cancels"]}
                venue.orders = [o for o in venue.orders if o["oid"] not in old]
            return {"status": "ok"}
        with patch("tools.local_operator.protection.send", side_effect=submit):
            result = execute_protect(self.venue, plan, "unused", self.root)
        self.assertEqual(result["status"], "protection_verified")
        self.assertEqual({o["oid"] for o in self.venue.orders}, {700, 701})

    def test_stop_loosening_and_liquidation_crossing_are_refused(self):
        with self.assertRaises(ValueError):
            plan_protect(self.venue, self.record, AGENT, "fixture", "80", "115")
        self.venue.position["liquidationPx"] = "110"
        with self.assertRaises(ValueError):
            self.plan()
