import copy
from decimal import Decimal
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from .signer import now_ms
from .store import read
from .venue import fresh_action_evidence, snapshot
from .workflow import adoption, execute_close, plan_close, reconcile, seal_plan


ACCOUNT = "0x" + "11" * 20
AGENT = "0x" + "22" * 20


class FakeVenue:
    network = "testnet"

    def __init__(self):
        self.position = {"coin": "BTC", "szi": "-0.1", "entryPx": "100",
                         "positionValue": "10", "unrealizedPnl": "0",
                         "leverage": {"type": "isolated", "value": 1, "rawUsd": "20"}}
        self.orders = [{"coin": "BTC", "side": "B", "oid": 123, "sz": "0.1", "limitPx": "121",
                        "triggerPx": "110", "reduceOnly": True, "isTrigger": True,
                        "orderType": "Stop Market", "children": []},
                       {"coin": "BTC", "side": "B", "oid": 456, "sz": "0.1", "limitPx": "88",
                        "triggerPx": "80", "reduceOnly": True, "isTrigger": True,
                        "orderType": "Take Profit Market", "children": []}]
        self.order_status = "filled"
        self.stale = False
        self.calls = []

    def info(self, kind, **fields):
        self.calls.append((kind, fields))
        if kind == "perpDexs":
            return [None, {"name": "xyz"}]
        if kind == "clearinghouseState":
            return {"time": now_ms() - (100_000 if self.stale else 0),
                    "assetPositions": [{"position": copy.deepcopy(self.position)}] if self.position and not fields.get("dex") else []}
        if kind == "frontendOpenOrders":
            return copy.deepcopy(self.orders) if not fields.get("dex") else []
        if kind == "spotClearinghouseState":
            return {"balances": [{"coin": "USDC", "token": 0, "total": "1000", "hold": "10"}],
                    "tokenToAvailableAfterMaintenance": [[0, "990"]]}
        if kind == "meta":
            return {"universe": [{"name": "BTC", "szDecimals": 3, "maxLeverage": 20}]}
        if kind == "l2Book":
            return {"coin": "BTC", "time": now_ms(), "levels": [[{"px": "99", "sz": "10"}], [{"px": "101", "sz": "10"}]]}
        if kind == "extraAgents":
            return [{"address": AGENT, "validUntil": now_ms() + 100_000}]
        if kind == "orderStatus":
            return {"status": "unknownOid"} if self.order_status is None else {"status": "order", "order": {"status": self.order_status}}
        raise AssertionError(kind)


class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.venue = FakeVenue()
        self.record = adoption(self.venue, ACCOUNT, "BTC")
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def plan(self, quantity=None):
        return plan_close(self.venue, self.record, AGENT, "fixture", quantity)

    def test_adoption_retains_unknown_history_and_exact_exit_ownership(self):
        self.assertFalse(self.record["history_complete"])
        self.assertEqual(self.record["origin"], "adopted_existing_position")
        self.assertEqual([o["oid"] for o in self.record["owned_exits"]], [123, 456])

    def test_close_plan_full_partial_precision_and_no_mainnet(self):
        self.assertEqual(self.plan()["action"]["orders"][0]["s"], "0.1")
        for quantity in ("0.2", "0", "0.0001"):
            with self.assertRaises(ValueError):
                self.plan(quantity)
        self.venue.position["szi"] = "-0.4"
        self.record = adoption(self.venue, ACCOUNT, "BTC")
        self.assertEqual(self.plan("0.2")["action"]["orders"][0]["s"], "0.2")
        self.venue.network = "mainnet"
        with self.assertRaises(ValueError):
            self.plan()

    def test_resize_or_unmanaged_order_blocks_before_send(self):
        plan = self.plan()
        self.venue.position["szi"] = "-0.2"
        with patch("tools.local_operator.workflow.send") as send:
            with self.assertRaises(ValueError):
                execute_close(self.venue, plan, "unused", self.root)
            send.assert_not_called()
        self.venue.position["szi"] = "-0.1"
        self.venue.orders[0]["oid"] = 789
        with self.assertRaises(ValueError):
            self.plan()

    def test_partial_close_keeps_protection(self):
        self.venue.position.update(szi="-0.4", positionValue="40")
        for order in self.venue.orders:
            order["sz"] = "0.4"
        self.record = adoption(self.venue, ACCOUNT, "BTC")
        plan = self.plan("0.2")
        def submit(*args, **kwargs):
            self.venue.position["szi"] = "-0.2"
            return {"status": "ok"}
        with patch("tools.local_operator.workflow.send", side_effect=submit) as send:
            result = execute_close(self.venue, plan, "unused", self.root)
            self.assertEqual(result["status"], "residual_position")
            self.assertEqual(send.call_count, 1)
        self.assertEqual(len(self.venue.orders), 2)

    def test_close_preflight_uses_approved_limit_for_both_sides(self):
        for is_long in (False, True):
            self.venue = FakeVenue()
            if is_long:
                self.venue.position["szi"] = "0.1"
                self.venue.orders[0].update(side="A", triggerPx="90", limitPx="81")
                self.venue.orders[1].update(side="A", triggerPx="120", limitPx="108")
            self.record = adoption(self.venue, ACCOUNT, "BTC")
            plan = self.plan()
            approved = Decimal(plan["action"]["orders"][0]["p"])
            original = self.venue.info
            for inside in (True, False):
                delta = Decimal("0.01") * (1 if is_long else -1)
                quote = approved + delta if inside else approved - delta
                def info(kind, **fields):
                    value = original(kind, **fields)
                    if kind == "l2Book":
                        value["levels"][0 if is_long else 1][0]["px"] = str(quote)
                    return value
                state = self.root / (str(is_long) + str(inside))
                with self.subTest(is_long=is_long, inside=inside), \
                        patch.object(self.venue, "info", side_effect=info), \
                        patch("tools.local_operator.workflow.send", side_effect=RuntimeError("preflight accepted")) as send:
                    if inside:
                        with self.assertRaisesRegex(RuntimeError, "preflight accepted"):
                            execute_close(self.venue, plan, "unused", state)
                        self.assertEqual(send.call_count, 1)
                    else:
                        with self.assertRaisesRegex(ValueError, "fresh depth"):
                            execute_close(self.venue, plan, "unused", state)
                        send.assert_not_called()

    def test_close_preflight_rejects_insufficient_approved_price_depth(self):
        plan = self.plan()
        original = self.venue.info
        def info(kind, **fields):
            value = original(kind, **fields)
            if kind == "l2Book":
                value["levels"][1][0]["sz"] = "0.001"
            return value
        with patch.object(self.venue, "info", side_effect=info), patch("tools.local_operator.workflow.send") as send:
            with self.assertRaisesRegex(ValueError, "fresh depth"):
                execute_close(self.venue, plan, "unused", self.root)
            send.assert_not_called()

    def test_full_close_cleans_only_owned_exits_after_flatness(self):
        plan = self.plan()
        def submit(venue, policy, action, *args, **kwargs):
            if action["type"] == "order":
                venue.position = None
            else:
                self.assertIsNone(venue.position)
                self.assertEqual({c["o"] for c in action["cancels"]}, {123, 456})
                venue.orders = []
            return {"status": "ok"}
        with patch("tools.local_operator.workflow.send", side_effect=submit):
            result = execute_close(self.venue, plan, "unused", self.root)
        self.assertEqual(result["status"], "closed")
        with self.assertRaises(ValueError):
            execute_close(self.venue, plan, "unused", self.root)

    def test_uncertain_submission_is_not_retried(self):
        plan = self.plan()
        with patch("tools.local_operator.workflow.send", side_effect=TimeoutError) as send:
            with self.assertRaises(TimeoutError):
                execute_close(self.venue, plan, "unused", self.root)
            with self.assertRaises(ValueError):
                execute_close(self.venue, plan, "unused", self.root)
            self.assertEqual(send.call_count, 1)
        self.venue.order_status = None
        self.assertEqual(reconcile(self.venue, plan)["status"], "reconciliation_required")

    def test_stale_flat_builder_state_fails_entire_snapshot(self):
        self.venue.stale = True
        with self.assertRaises(ValueError):
            snapshot(self.venue, ACCOUNT)

    def test_mutated_exit_prevents_adoption(self):
        self.venue.orders[0]["reduceOnly"] = False
        with self.assertRaises(ValueError):
            adoption(self.venue, ACCOUNT, "BTC")

    def test_plan_risk_or_action_mutation_is_rejected_before_effects(self):
        plan = self.plan()
        plan["slippage_bps"] = 100
        with patch("tools.local_operator.workflow.send") as submit:
            with self.assertRaises(ValueError):
                execute_close(self.venue, plan, "unused", self.root)
            submit.assert_not_called()


class CloseMinimumTests(unittest.TestCase):
    def venue(self, is_long=True, quantity="0.0008", quote="25000"):
        venue = FakeVenue()
        venue.position.update(szi=quantity if is_long else "-"+quantity,
                              entryPx="24000", positionValue=str(Decimal(quantity)*Decimal(quote)))
        for order, trigger in zip(venue.orders, ("22000", "26000") if is_long else ("26000", "22000")):
            order.update(side="A" if is_long else "B", sz=quantity, triggerPx=trigger, limitPx=trigger)
        original = venue.info
        def info(kind, **fields):
            if kind == "meta":
                return {"universe": [{"name": "BTC", "szDecimals": 5, "maxLeverage": 40}]}
            if kind == "l2Book":
                return {"coin": "BTC", "time": now_ms(), "levels": [[{"px": quote, "sz": "10"}], [{"px": quote, "sz": "10"}]]}
            return original(kind, **fields)
        venue.info = info
        return venue

    def plan(self, venue, quantity=None, slippage_bps=0):
        return plan_close(venue, adoption(venue, ACCOUNT, "BTC"), AGENT, "fixture", quantity, slippage_bps)

    def test_subminimum_partial_rejection_and_valid_larger_close_for_both_sides(self):
        for is_long in (True, False):
            with self.subTest(is_long=is_long):
                venue = self.venue(is_long)
                with self.assertRaisesRegex(ValueError, "at least 10 USDC"):
                    self.plan(venue, "0.0002")
                plan = self.plan(venue, "0.0005")
                self.assertEqual(plan["action"]["orders"][0]["s"], "0.0005")
                fresh_action_evidence(venue, plan["policy"], plan["action"])

    def test_exact_minimum_and_just_below_use_decimal_order_limit(self):
        for is_long in (True, False):
            for quote in ("49999", "50000", "50001"):
                with self.subTest(is_long=is_long, quote=quote):
                    venue = self.venue(is_long, quantity="0.0004", quote=quote)
                    if quote == "49999":
                        with self.assertRaisesRegex(ValueError, "at least 10 USDC"):
                            self.plan(venue, "0.0002")
                    else:
                        plan = self.plan(venue, "0.0002")
                        fresh_action_evidence(venue, plan["policy"], plan["action"])
        # The book quote alone meets 10 USDC, but the rounded sell collar does not.
        with self.assertRaisesRegex(ValueError, "at least 10 USDC"):
            self.plan(self.venue(quantity="0.0004", quote="50000"), "0.0002", slippage_bps=30)

    def test_full_residual_below_minimum_remains_executable_for_both_sides(self):
        for is_long in (True, False):
            venue = self.venue(is_long, quantity="0.0001", quote="25000")
            for quantity in (None, "0.0001"):
                with self.subTest(is_long=is_long, quantity=quantity):
                    plan = self.plan(venue, quantity)
                    with tempfile.TemporaryDirectory() as temporary, \
                            patch("tools.local_operator.workflow.send", side_effect=RuntimeError("preflight accepted")) as send:
                        with self.assertRaisesRegex(RuntimeError, "preflight accepted"):
                            execute_close(venue, plan, "unused", Path(temporary).resolve())
                        send.assert_called_once()

    def test_old_sealed_subminimum_plan_fails_before_signing_or_reservation(self):
        venue = self.venue()
        plan = self.plan(venue)
        plan["policy"]["quantity"] = "0.0002"
        plan["action"]["orders"][0]["s"] = "0.0002"
        seal_plan(plan)  # Models a valid immutable plan prepared before the fix.
        with tempfile.TemporaryDirectory() as temporary, patch("tools.local_operator.workflow.send") as send:
            root = Path(temporary).resolve()
            with self.assertRaisesRegex(ValueError, "at least 10 USDC"):
                execute_close(venue, plan, "unused", root)
            send.assert_not_called()
            self.assertEqual(list(root.rglob("*.json")), [])

    def test_fresh_exposure_decides_full_close_exception(self):
        venue = self.venue(quantity="0.0001")
        plan = self.plan(venue)
        fresh_action_evidence(venue, plan["policy"], plan["action"])
        venue.position["szi"] = "0.0008"
        with self.assertRaisesRegex(ValueError, "at least 10 USDC"):
            fresh_action_evidence(venue, plan["policy"], plan["action"])
        venue.position["szi"] = "0.00005"
        with self.assertRaisesRegex(ValueError, "exceeds fresh residual"):
            fresh_action_evidence(venue, plan["policy"], plan["action"])
