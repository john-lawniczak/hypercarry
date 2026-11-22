from unittest.mock import patch
import unittest
from .equity import reconcile_equity, checkpoint
from .test_ledger import LedgerVenue, fill
from .test_workflow import ACCOUNT


def point(start, end, cash, pnl):
    return {"schema_version": 1, "kind": "unified_usdc_checkpoint", "network": "testnet", "account": ACCOUNT,
            "started_at_ms": start, "completed_at_ms": end, "account_mode": "unifiedAccount",
            "model_id": "unified_marked_balance_v1", "shared_usdc_balance": cash, "unrealized_perp_pnl_usdc": pnl, "unsupported_components": []}


class EquityTests(unittest.TestCase):
    def setUp(self):
        self.venue = LedgerVenue()
        self.before = point(100, 110, "100", "-3")
        self.after = point(200, 210, "157.4", "2")
        self.ledger = {"network": "testnet", "account": ACCOUNT,
            "windows": [{"start_ms": 100, "end_ms": 210}],
            "records": {"fills": {"f": fill(1, 150)}, "funding": {},
                "transfers": {"t": {"time": 160, "delta": {"type": "deposit", "usdc": "50"}}}}}

    def test_cash_mark_and_capital_are_separated(self):
        r = reconcile_equity(self.venue, self.ledger, self.before, self.after)
        self.assertEqual(r["status"], "matched_observed_window")
        self.assertEqual(r["external_capital_usdc"], "50")
        self.assertEqual(r["observed_realized_net_usdc"], "2.4")
        self.assertEqual(r["marked_equity_change_usdc"], "57.4")
        self.assertFalse(r["mainnet_authorized"])

    def test_residual_is_retained_not_adjusted_away(self):
        self.after["shared_usdc_balance"] = "158.4"
        r = reconcile_equity(self.venue, self.ledger, self.before, self.after)
        self.assertEqual(r["unexplained_residual_usdc"], "1.0")
        self.assertEqual(r["status"], "unexplained_residual")

    def test_send_deposit_and_funding_window_reconciles_without_capital_as_profit(self):
        self.ledger["records"]["transfers"]["t"]["delta"] = {
            "type": "send", "user": "0x" + "33" * 20, "destination": ACCOUNT,
            "sourceDex": "spot", "destinationDex": "", "token": "USDC",
            "amount": "50", "usdcValue": "50", "fee": "0", "nativeTokenFee": "0"}
        self.ledger["records"]["funding"]["f"] = {
            "time": 170, "delta": {"type": "funding", "coin": "BTC", "usdc": "0.2"}}
        self.after["shared_usdc_balance"] = "157.6"
        result = reconcile_equity(self.venue, self.ledger, self.before, self.after)
        self.assertEqual(result["status"], "matched_observed_window")
        self.assertEqual(result["external_capital_usdc"], "50")
        self.assertEqual(result["observed_realized_net_usdc"], "2.6")
        self.assertEqual(result["unexplained_residual_usdc"], "0.0")
        self.ledger["records"]["transfers"]["t"]["delta"]["fee"] = "1"
        self.assertEqual(reconcile_equity(self.venue, self.ledger, self.before, self.after)["status"], "incomplete")

    def test_boundary_events_and_coverage_gaps_prevent_matched_claim(self):
        self.ledger["records"]["fills"]["f"]["time"] = 205
        r = reconcile_equity(self.venue, self.ledger, self.before, self.after)
        self.assertEqual(r["status"], "incomplete")
        self.assertTrue(r["boundary_events"])
        self.ledger["windows"] = []
        self.assertIn("ledger window not enumerated", reconcile_equity(self.venue, self.ledger, self.before, self.after)["gaps"])

    def test_aggregate_outside_window_cannot_silently_disappear_from_equity_gaps(self):
        self.ledger["records"]["funding"]["daily"] = {"time": 0,
            "delta": {"type": "funding", "coin": "BTC", "usdc": "0.1", "nSamples": 24}}
        result = reconcile_equity(self.venue, self.ledger, self.before, self.after)
        self.assertEqual(result["status"], "incomplete")
        self.assertIn("aggregated funding requires independent window reconstruction", result["gaps"])

    def test_checkpoint_interval_excludes_start_and_refuses_wrong_identity(self):
        self.ledger["records"]["fills"]["old"] = fill(2, 110)
        r = reconcile_equity(self.venue, self.ledger, self.before, self.after)
        self.assertEqual(r["observed_realized_net_usdc"], "2.4")
        self.after["account"] = "0x" + "55" * 20
        with self.assertRaises(ValueError): reconcile_equity(self.venue, self.ledger, self.before, self.after)

    def test_checkpoint_does_not_add_unrealized_pnl_to_unified_balance_again(self):
        observation = {"balance": {"balances": [{"token": 0, "coin": "USDC", "total": "97"}]},
            "dexs": [{"dex": "", "state": {"assetPositions": [{"position": {
                "coin": "BTC", "szi": "1", "unrealizedPnl": "-3"}}]}}]}
        def info(kind, **fields):
            return "unifiedAccount" if kind == "userAbstraction" else {"collateralToken": 0}
        with patch("tools.local_operator.equity.snapshot", return_value=observation), patch.object(self.venue, "info", side_effect=info):
            p = checkpoint(self.venue, ACCOUNT)
        self.assertEqual(p["marked_usdc_equity_model"], "97")
        self.assertEqual(p["unrealized_perp_pnl_usdc"], "-3")
