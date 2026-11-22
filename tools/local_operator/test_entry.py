from decimal import Decimal
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from .entry import execute_open, plan_open, reconcile_open
from .signer import now_ms
from .store import write
from .test_workflow import ACCOUNT, AGENT, FakeVenue


ROOT = Path(__file__).resolve().parents[2]
ANALYTICS = ROOT / "target/debug/hypercarry"
JOURNAL = ROOT / "target/debug/hypercarry-bracket-journal"


class EntryVenue(FakeVenue):
    def __init__(self):
        super().__init__()
        self.position = None
        self.orders = []

    def info(self, kind, **fields):
        if kind == "meta":
            return {"universe": [{"name": "BTC", "szDecimals": 3, "maxLeverage": 20, "marginTableId": 20}]}
        if kind == "metaAndAssetCtxs":
            return [self.info("meta"), [{"markPx": "101"}]]
        if kind == "userFees":
            return {"userCrossRate": "0.00045"}
        if kind == "userAbstraction":
            return "unifiedAccount"
        if kind == "activeAssetData":
            return {"user": ACCOUNT, "coin": "BTC", "leverage": {"type": "isolated", "value": 1},
                    "maxTradeSzs": ["100", "100"]}
        return super().info(kind, **fields)


@unittest.skipUnless(ANALYTICS.exists() and JOURNAL.exists(), "build Rust CLI and bracket journal bridge")
class EntryTests(unittest.TestCase):
    def setUp(self):
        self.venue = EntryVenue()
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.input = {"network": "testnet", "account": ACCOUNT, "coin": "BTC", "direction": "long",
                      "notional_budget": "20", "leverage": 1, "reserve_usdc": "400",
                      "entry_slippage_percent": "0.5", "exit_slippage_percent": "10",
                      "take_profit": "120", "stop_loss": "90", "min_health_at_stop": "2"}

    def plan(self):
        return plan_open(self.venue, self.input, AGENT, "fixture", ANALYTICS, JOURNAL)

    def submitter(self, partial=False, unknown=False):
        def submit(venue, policy, action, socket_path, attempt, label, before_sign=None, before_submit=None):
            if before_sign:
                before_sign(now_ms())
            if before_submit:
                before_submit()
            if label == "configure":
                return {"status": "ok"}
            if label == "entry":
                quantity = Decimal(policy["quantity"]) / 2 if partial else Decimal(policy["quantity"])
                venue.position = {"coin": "BTC", "szi": str(quantity), "entryPx": "101", "positionValue": str(quantity*101),
                                  "leverage": {"type": "isolated", "value": 1, "rawUsd": "0"}}
                if unknown:
                    venue.order_status = None
                    return {"status": "ok"}
                if partial:
                    return {"status": "ok"}
            for order in action["orders"]:
                if not order["r"]:
                    continue
                trigger = order["t"]["trigger"]
                venue.orders.append({"coin": "BTC", "oid": len(venue.orders) + 1, "cloid": order["c"],
                                     "sz": order["s"], "side": "B" if order["b"] else "A",
                                     "limitPx": order["p"], "triggerPx": trigger["triggerPx"],
                                     "reduceOnly": True, "isTrigger": True, "children": [],
                                     "orderType": "Take Profit Market" if trigger["tpsl"] == "tp" else "Stop Market"})
            return {"status": "ok"}
        return submit

    def test_real_rust_calculation_and_journal_full_fill(self):
        plan = self.plan()
        self.assertFalse(plan["calculated"]["executable"])
        with patch("tools.local_operator.entry.send", side_effect=self.submitter()):
            result = execute_open(self.venue, plan, "unused", self.root)
        self.assertEqual(result["status"], "open")
        self.assertTrue((self.root / plan["policy"]["session_id"] / "bracket.jsonl").exists())

    def test_recovery_freeze_survives_host_exit_and_prevents_entry(self):
        plan = self.plan()
        (self.root / ("testnet-" + ACCOUNT + ".entries-disabled")).touch()
        with patch("tools.local_operator.entry.send") as send:
            with self.assertRaisesRegex(ValueError, "frozen"):
                execute_open(self.venue, plan, "unused", self.root)
            send.assert_not_called()

    def test_entry_preflight_uses_approved_limit_for_both_sides(self):
        for direction in ("long", "short"):
            self.input.update(direction=direction, take_profit="120" if direction == "long" else "90",
                              stop_loss="90" if direction == "long" else "120")
            plan = self.plan()
            limit = Decimal(plan["policy"]["entry_limit"])
            original = self.venue.info
            for inside in (True, False):
                delta = Decimal("0.01") * (1 if direction == "long" else -1)
                quote = limit - delta if inside else limit + delta
                def info(kind, **fields):
                    value = original(kind, **fields)
                    if kind == "l2Book":
                        value["levels"][1 if direction == "long" else 0][0]["px"] = str(quote)
                    return value
                state = self.root / (direction + str(inside))
                with self.subTest(direction=direction, inside=inside), \
                        patch.object(self.venue, "info", side_effect=info), \
                        patch("tools.local_operator.entry.send", side_effect=RuntimeError("preflight accepted")) as send:
                    if inside:
                        with self.assertRaisesRegex(RuntimeError, "preflight accepted"):
                            execute_open(self.venue, plan, "unused", state)
                        self.assertEqual(send.call_count, 1)
                    else:
                        with self.assertRaisesRegex(ValueError, "fresh depth"):
                            execute_open(self.venue, plan, "unused", state)
                        send.assert_not_called()

    def test_entry_preflight_rejects_insufficient_depth_at_approved_limit(self):
        plan = self.plan()
        original = self.venue.info
        def info(kind, **fields):
            value = original(kind, **fields)
            if kind == "l2Book":
                value["levels"][1][0]["sz"] = "0.001"
            return value
        with patch.object(self.venue, "info", side_effect=info), patch("tools.local_operator.entry.send") as send:
            with self.assertRaisesRegex(ValueError, "fresh depth"):
                execute_open(self.venue, plan, "unused", self.root)
            send.assert_not_called()

    def test_wrong_account_capacity_does_not_open_a_position(self):
        plan = self.plan()
        original = self.venue.info
        def info(kind, **fields):
            value = original(kind, **fields)
            if kind == "activeAssetData":
                value["user"] = "0x" + "55" * 20
            return value
        with patch.object(self.venue, "info", side_effect=info), patch("tools.local_operator.entry.send", side_effect=self.submitter()):
            with self.assertRaisesRegex(ValueError, "identity"):
                execute_open(self.venue, plan, "unused", self.root)
        self.assertIsNone(self.venue.position)
        self.assertTrue((self.root / ("testnet-" + ACCOUNT + "-pending.json")).exists())

    def test_recovery_handoff_uses_replayed_partial_fill_exit_ids(self):
        from .recovery import prepare
        from .store import read
        plan = self.plan()
        with patch("tools.local_operator.entry.send", side_effect=self.submitter(partial=True)):
            execute_open(self.venue, plan, "unused", self.root)
        limits = {"size_step": "0.001", "price_tick": "0.001", "min_price": "50", "max_price": "150",
                  "slippage_bps": 50, "stop_grace_ms": 0, "max_snapshot_age_ms": 10000,
                  "max_close_attempts": 3, "max_cancel_attempts": 4, "retry_ms": 15000,
                  "recovery_timeout_ms": 60000, "action_ttl_ms": 10000}
        handoff = prepare(self.venue, plan, self.root, limits, JOURNAL, self.root / "agent.sock")
        ids = handoff["recovery_policy"]["repaired_exits"]
        self.assertEqual(handoff["policy"]["recovery_cloids"][1:], [ids["tp"], ids["sl"]])
        self.assertFalse(handoff["policy"]["allow_entry"])
        self.assertEqual(Decimal(handoff["policy"]["quantity"]), Decimal(plan["policy"]["quantity"]) / 2)

    def test_partial_fill_repairs_actual_size_and_records_real_replacement_ids(self):
        plan = self.plan()
        with patch("tools.local_operator.entry.send", side_effect=self.submitter(partial=True)) as send:
            result = execute_open(self.venue, plan, "unused", self.root)
            self.assertEqual(send.call_count, 3)
        self.assertEqual(result["status"], "open")
        self.assertTrue(all(Decimal(o["sz"]) == Decimal(self.venue.position["szi"]) for o in self.venue.orders))

    def test_unknown_entry_outcome_never_repairs_or_retries(self):
        plan = self.plan()
        with patch("tools.local_operator.entry.send", side_effect=self.submitter(unknown=True)) as send:
            result = execute_open(self.venue, plan, "unused", self.root)
            self.assertEqual(result["status"], "reconciliation_required")
            self.assertEqual(send.call_count, 2)
            with self.assertRaises(ValueError):
                execute_open(self.venue, plan, "unused", self.root)

    def test_entry_refuses_insufficient_reserve_or_existing_target(self):
        self.input["reserve_usdc"] = "10000"
        with self.assertRaises(ValueError):
            self.plan()
        self.input["reserve_usdc"] = "400"
        self.venue.position = FakeVenue().position
        with self.assertRaises(ValueError):
            self.plan()

    def rejected_attempt(self, response, broadcast=True):
        plan = self.plan()
        self.venue.order_status = None
        def submit(venue, policy, action, socket_path, attempt, label, before_sign=None, before_submit=None):
            if label == "configure":
                return {"status": "ok"}
            nonce = now_ms()
            before_sign(nonce)
            write(attempt / "entry-intent.json", {"network": "testnet", "key_id": policy["key_id"],
                  "signer_address": policy["signer_address"], "nonce": nonce,
                  "expires_after": nonce + 1000, "action": action})
            if broadcast:
                write(attempt / "entry-broadcast.json", {"nonce": nonce, "at_ms": nonce})
            if response is not None:
                write(attempt / "entry-response.json", response)
            return response
        with patch("tools.local_operator.entry.send", side_effect=submit):
            result = execute_open(self.venue, plan, "unused", self.root)
        self.assertEqual(result["status"], "reconciliation_required")
        return plan

    def test_explicit_expiry_rejection_reconciles_flat_without_replaying_entry(self):
        plan = self.rejected_attempt({"status": "err", "response": "Action already expired"})
        with patch("tools.local_operator.entry.now_ms", return_value=now_ms() + 2000), \
                patch("tools.local_operator.entry.send") as send:
            result = reconcile_open(self.venue, plan, self.root)
            send.assert_not_called()
        self.assertEqual(result["status"], "closed")
        self.assertFalse((self.root / ("testnet-" + ACCOUNT + "-pending.json")).exists())
        self.assertTrue(list((self.root / plan["policy"]["session_id"]).glob("entry-rejection-*.json")))
        with self.assertRaisesRegex(ValueError, "already reserved"):
            execute_open(self.venue, plan, "unused", self.root)

    def test_never_broadcast_entry_reconciles_only_after_signature_expiry(self):
        plan = self.rejected_attempt(None, broadcast=False)
        self.assertEqual(reconcile_open(self.venue, plan, self.root)["status"], "reconciliation_required")
        with patch("tools.local_operator.entry.now_ms", return_value=now_ms() + 2000):
            self.assertEqual(reconcile_open(self.venue, plan, self.root)["status"], "closed")

    def test_missing_or_unrecognized_response_never_clears_flat_reservation(self):
        for response in (None, {"status": "err", "response": "unknown failure"}):
            with self.subTest(response=response):
                plan = self.rejected_attempt(response)
                with patch("tools.local_operator.entry.now_ms", return_value=now_ms() + 2000):
                    self.assertEqual(reconcile_open(self.venue, plan, self.root)["status"], "reconciliation_required")
                self.assertTrue((self.root / ("testnet-" + ACCOUNT + "-pending.json")).exists())
                # A separate account-state directory for each independent fixture.
                self.root = self.root / "next"

    def test_expiry_rejection_with_orphan_exit_cannot_clear_reservation(self):
        plan = self.rejected_attempt({"status": "err", "response": "Action already expired"})
        self.venue.orders = FakeVenue().orders
        with patch("tools.local_operator.entry.now_ms", return_value=now_ms() + 2000):
            self.assertNotEqual(reconcile_open(self.venue, plan, self.root)["status"], "closed")
        self.assertTrue((self.root / ("testnet-" + ACCOUNT + "-pending.json")).exists())
