"""Overlapping account-ledger ingestion with explicit retention/coverage limits."""
from decimal import Decimal
from pathlib import Path
import uuid

from .signer import address, now_ms, number
from .store import digest, lock, private_dir, read, require, write
from .venue import dex_fields


KINDS = {"fills": ("userFillsByTime", 2000), "funding": ("userFunding", 500),
         "transfers": ("userNonFundingLedgerUpdates", 500)}


def row_id(kind, row):
    require(type(row["time"]) is int and row["time"] >= 0, "invalid ledger timestamp")
    if kind == "fills":
        return digest((row["coin"], row["tid"], row["oid"], row["hash"]))
    delta = row["delta"]
    return digest((row["time"], row["hash"], delta["type"], delta.get("coin")))


def collect(venue, account, kind, start, end, save_page=None):
    endpoint, cap = KINDS[kind]
    rows = {}
    cursor = start
    for page_number in range(1000):
        fields = {"user": account, "startTime": cursor, "endTime": end}
        if kind == "fills":
            fields["aggregateByTime"] = False
        page = venue.info(endpoint, **fields)
        require(isinstance(page, list) and len(page) <= cap, "invalid/oversized ledger page")
        if save_page:
            save_page(kind, page_number, fields, page)
        for row in page:
            require(cursor <= row["time"] <= end, "ledger row outside requested inclusive window")
            key = row_id(kind, row)
            require(key not in rows or rows[key] == row, "conflicting duplicate in ledger pages")
            rows[key] = row
        if len(page) < cap:
            return rows
        boundary = max(row["time"] for row in page)
        require(boundary > cursor, "pagination stalled at a saturated timestamp; coverage unknown")
        cursor = boundary  # Inclusive overlap: never skip other events in the same millisecond.
    raise ValueError("ledger pagination budget exhausted")


def sync(venue, account, directory, start, end):
    address(account)
    require(type(start) is int and type(end) is int and 0 <= start <= end <= now_ms(), "invalid ledger window")
    root = private_dir(directory)
    with lock(root / "ledger.lock"):
        target = root / "ledger.json"
        ledger = read(target) if target.exists() else {"schema_version": 1, "network": venue.network,
            "account": account, "records": {key: {} for key in KINDS}, "windows": []}
        require(ledger["network"] == venue.network and ledger["account"] == account and ledger["schema_version"] == 1,
                "ledger identity/schema mismatch")
        batch = private_dir(root / ("batch-" + uuid.uuid4().hex))
        def save_page(kind, index, fields, page):
            write(batch / (kind + "-" + str(index) + ".json"), {"request": fields, "response": page,
                                                                         "observed_at_ms": now_ms()})
        try:
            for kind in KINDS:
                for key, row in collect(venue, account, kind, start, end, save_page).items():
                    previous = ledger["records"][kind].get(key)
                    require(previous is None or previous == row, "conflicting persisted ledger event")
                    ledger["records"][kind][key] = row
            window = {"start_ms": start, "end_ms": end, "observed_at_ms": now_ms(), "batch": batch.name,
                      "pagination_completed": True, "historical_completeness": "unproven",
                      "fills_retention_limit": 10000}
            ledger["windows"].append(window)
            write(target, ledger, replace=True)
            write(batch / "receipt.json", {"ledger_sha256": digest(ledger), "window": window})
        except Exception:
            write(batch / "failed.json", {"at_ms": now_ms(), "coverage": "unknown", "checkpoint_advanced": False})
            raise
        return ledger


def send_capital(delta, account):
    """Classify only unambiguous zero-fee USDC sends within modeled cash scope.

    Fee-bearing sends and other DEX/token routes remain explicit gaps until
    their debit semantics and valuation have independent evidence.
    """
    try:
        sender, recipient = delta["user"].lower(), delta["destination"].lower()
        address(sender)
        address(recipient)
        if (delta["token"] != "USDC" or delta["sourceDex"] not in ("", "spot")
                or delta["destinationDex"] not in ("", "spot")
                or number(delta["fee"]) != 0 or number(delta["nativeTokenFee"]) != 0):
            return None
        amount = number(delta["amount"])
        if amount < 0 or number(delta["usdcValue"]) != amount:
            return None
        owner = account.lower()
        if sender == owner and recipient == owner:
            return Decimal(0)  # Internal cash movement, never new contributed capital.
        if recipient == owner:
            return amount
        if sender == owner:
            return -amount
    except (KeyError, AttributeError, TypeError, ValueError):
        pass
    return None


def report(venue, ledger, start, end):
    require(ledger["network"] == venue.network and start <= end, "report scope mismatch")
    rows = {kind: [row for row in values.values() if start <= row["time"] <= end]
            for kind, values in ledger["records"].items()}
    # Compacted funding timestamps label aggregates, not individual settlements.
    # They can precede entry and overlap archived hourly events under different
    # IDs. Do not sum these into a qualified net or assume a filtered window is
    # complete. Retain all source records for independent reconstruction.
    aggregate_coins = {r["delta"]["coin"] for r in ledger["records"]["funding"].values()
                       if r["delta"].get("nSamples") is not None}
    coins = {row["coin"] for row in rows["fills"]} | {row["delta"]["coin"] for row in rows["funding"]} | aggregate_coins
    usdc = {}
    for coin in sorted(coins):
        if coin.startswith("@") or "/" in coin:
            usdc[coin] = False  # Spot cost-basis accounting is a separate leg.
        else:
            meta = venue.info("meta", **dex_fields(coin))
            usdc[coin] = meta.get("collateralToken", 0 if ":" not in coin else None) == 0
    markets = {}
    fees = {}
    def market(coin):
        return markets.setdefault(coin, {"realized_price_pnl": Decimal(0), "funding": Decimal(0), "fees_usdc": Decimal(0),
                                         "usdc_collateral_verified": usdc[coin],
                                         "funding_aggregation_unresolved": coin in aggregate_coins})
    for coin in aggregate_coins:
        market(coin)
    for row in rows["fills"]:
        item = market(row["coin"])
        item["realized_price_pnl"] += number(row["closedPnl"])
        token = row["feeToken"].strip()
        fee = number(row["fee"])
        fees[token] = fees.get(token, Decimal(0)) + fee
        if token == "USDC":
            item["fees_usdc"] += fee
        else:
            item["usdc_collateral_verified"] = False
    for row in rows["funding"]:
        market(row["delta"]["coin"])["funding"] += number(row["delta"]["usdc"])
    known_net = Decimal(0)
    for item in markets.values():
        net = item["realized_price_pnl"] + item["funding"] - item["fees_usdc"]
        item["known_net_usdc"] = net if item["usdc_collateral_verified"] and not item["funding_aggregation_unresolved"] else None
        if item["known_net_usdc"] is not None:
            known_net += net
    capital = Decimal(0)
    unclassified = []
    for row in rows["transfers"]:
        delta = row["delta"]
        if delta["type"] == "deposit":
            capital += number(delta["usdc"])
        elif delta["type"] == "withdraw":
            capital -= number(delta["usdc"])
        elif delta["type"] == "send" and (flow := send_capital(delta, ledger["account"])) is not None:
            capital += flow
        else:
            unclassified.append(delta["type"])
    covered_until = start
    covered = False
    for window in sorted(ledger["windows"], key=lambda w: w["start_ms"]):
        if window["end_ms"] < covered_until:
            continue
        if window["start_ms"] > covered_until:
            break
        covered_until = max(covered_until, window["end_ms"])
        if covered_until >= end:
            covered = True
            break
    return {"schema_version": 1, "network": venue.network, "account": ledger["account"],
            "start_ms": start, "end_ms": end, "historical_completeness": "unproven",
            "requested_window_enumerated": covered,
            "known_realized_net_usdc": str(known_net), "known_external_capital_usdc": str(capital),
            "fees_by_token": {key: str(value) for key, value in fees.items()},
            "markets": {coin: {key: str(value) if isinstance(value, Decimal) else value for key, value in item.items()}
                        for coin, item in markets.items()},
            "unclassified_ledger_types": sorted(set(unclassified)), "rows": {key: len(value) for key, value in rows.items()},
            "aggregated_funding_coins": sorted(aggregate_coins),
            "limitations": ["Observed realized cash flows only; not lifetime or equity-reconciled returns.",
                            "Open-position mark P&L, unclassified transfers and non-USDC/spot legs are not included in known net.",
                            "Compacted funding requires independent reconstruction; aggregate and hourly rows may overlap and are not qualified net cash flow.",
                            "Inclusive API pagination cannot recover fills beyond venue retention; archived history is required."]}
