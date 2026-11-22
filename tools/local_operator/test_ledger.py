import copy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from .ledger import collect, report, sync, send_capital
from .store import read
from .test_workflow import ACCOUNT


def fill(index, time=None):
    return {"time": index if time is None else time, "coin": "BTC", "tid": index, "oid": index,
            "hash": "hash-" + str(index), "closedPnl": "2.5", "fee": "0.1", "feeToken": "USDC"}


class LedgerVenue:
    network = "testnet"

    def __init__(self):
        self.rows = {"userFillsByTime": [fill(1), fill(2)],
                     "userFunding": [{"time": 2, "hash": "funding", "delta": {"type": "funding", "coin": "BTC", "usdc": "-0.2"}}],
                     "userNonFundingLedgerUpdates": [{"time": 1, "hash": "deposit", "delta": {"type": "deposit", "usdc": "100"}}]}
        self.fail = None

    def info(self, kind, **fields):
        if kind == self.fail:
            raise TimeoutError()
        if kind == "meta":
            return {"collateralToken": 0}
        return copy.deepcopy([r for r in self.rows[kind] if fields["startTime"] <= r["time"] <= fields["endTime"]])


class LedgerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.venue = LedgerVenue()

    def test_overlap_deduplicates_and_separates_capital_from_returns(self):
        sync(self.venue, ACCOUNT, self.root, 0, 3)
        ledger = sync(self.venue, ACCOUNT, self.root, 1, 4)
        self.assertEqual(len(ledger["records"]["fills"]), 2)
        result = report(self.venue, ledger, 0, 4)
        self.assertEqual(result["known_realized_net_usdc"], "4.6")
        self.assertEqual(result["known_external_capital_usdc"], "100")
        self.assertTrue(result["requested_window_enumerated"])
        self.assertEqual(result["historical_completeness"], "unproven")
        self.assertFalse(report(self.venue, ledger, 0, 5)["requested_window_enumerated"])

    def test_conflicting_duplicate_and_failed_read_do_not_advance_checkpoint(self):
        sync(self.venue, ACCOUNT, self.root, 0, 3)
        before = read(self.root / "ledger.json")
        self.venue.rows["userFillsByTime"][0]["fee"] = "9"
        with self.assertRaises(ValueError):
            sync(self.venue, ACCOUNT, self.root, 0, 4)
        self.assertEqual(read(self.root / "ledger.json"), before)
        self.venue.rows["userFillsByTime"][0]["fee"] = "0.1"
        self.venue.fail = "userFunding"
        with self.assertRaises(TimeoutError):
            sync(self.venue, ACCOUNT, self.root, 0, 4)
        self.assertEqual(read(self.root / "ledger.json"), before)

    def test_pagination_overlaps_and_refuses_saturated_timestamp(self):
        pages = [[fill(1), fill(2)], [fill(2), fill(3)], [fill(3)]]
        requests = []
        def page(kind, **fields):
            requests.append(fields["startTime"])
            return pages.pop(0)
        with patch.dict("tools.local_operator.ledger.KINDS", {"fills": ("userFillsByTime", 2)}):
            with patch.object(self.venue, "info", side_effect=page):
                rows = collect(self.venue, ACCOUNT, "fills", 0, 4)
            self.assertEqual(len(rows), 3)
            self.assertEqual(requests, [0, 2, 3])
            with patch.object(self.venue, "info", return_value=[fill(1, 0), fill(2, 0)]):
                with self.assertRaises(ValueError):
                    collect(self.venue, ACCOUNT, "fills", 0, 4)

    def test_unknown_currency_and_transfer_types_are_not_fabricated_pnl(self):
        self.venue.rows["userFillsByTime"][0]["feeToken"] = "USDH"
        self.venue.rows["userNonFundingLedgerUpdates"].append({"time": 2, "hash": "unknown", "delta": {"type": "futureType", "usdc": "999"}})
        ledger = sync(self.venue, ACCOUNT, self.root, 0, 3)
        result = report(self.venue, ledger, 0, 3)
        self.assertIsNone(result["markets"]["BTC"]["known_net_usdc"])
        self.assertEqual(result["known_realized_net_usdc"], "0")
        self.assertEqual(result["unclassified_ledger_types"], ["futureType"])

    def test_account_identity_cannot_share_checkpoint(self):
        sync(self.venue, ACCOUNT, self.root, 0, 3)
        with self.assertRaises(ValueError):
            sync(self.venue, "0x" + "33" * 20, self.root, 0, 4)

    def test_compacted_funding_never_double_counts_archived_hours_or_certifies_a_window(self):
        ledger = sync(self.venue, ACCOUNT, self.root, 0, 3)
        ledger["records"]["funding"]["daily"] = {"time": 0, "hash": "daily",
            "delta": {"type": "funding", "coin": "BTC", "usdc": "-0.2", "nSamples": 1}}
        for start in (0, 1):
            result = report(self.venue, ledger, start, 3)
            self.assertEqual(result["aggregated_funding_coins"], ["BTC"])
            self.assertTrue(result["markets"]["BTC"]["funding_aggregation_unresolved"])
            self.assertIsNone(result["markets"]["BTC"]["known_net_usdc"])
            self.assertEqual(result["known_realized_net_usdc"], "0")

    def test_usdc_send_direction_internal_moves_and_unknown_routes(self):
        other = "0x" + "33" * 20
        incoming = {"type": "send", "user": other, "destination": ACCOUNT,
                    "sourceDex": "spot", "destinationDex": "", "token": "USDC",
                    "amount": "50", "usdcValue": "50", "fee": "0", "nativeTokenFee": "0"}
        self.assertEqual(send_capital(incoming, ACCOUNT), 50)
        self.assertEqual(send_capital({**incoming, "user": ACCOUNT, "destination": other}, ACCOUNT), -50)
        self.assertEqual(send_capital({**incoming, "user": ACCOUNT}, ACCOUNT), 0)
        for change in ({"fee": "1"}, {"nativeTokenFee": "1"}, {"token": "USDH"},
                       {"sourceDex": "unmodeled"}, {"destinationDex": "vault"},
                       {"destination": other}, {"user": "bad"}, {"amount": "-1"},
                       {"usdcValue": "49"}, {"amount": "NaN"}):
            with self.subTest(change=change):
                self.assertIsNone(send_capital({**incoming, **change}, ACCOUNT))
        self.assertIsNone(send_capital({"type": "send"}, ACCOUNT))
        self.venue.rows["userNonFundingLedgerUpdates"] = [{"time": 1, "hash": "send", "delta": incoming}]
        ledger = sync(self.venue, ACCOUNT, self.root, 0, 3)
        result = report(self.venue, ledger, 0, 3)
        self.assertEqual(result["known_external_capital_usdc"], "50")
        self.assertEqual(result["known_realized_net_usdc"], "4.6")
        self.assertEqual(result["unclassified_ledger_types"], [])
