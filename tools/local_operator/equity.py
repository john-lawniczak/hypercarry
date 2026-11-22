"""Windowed Unified-USDC reconciliation with explicit valuation and coverage gaps."""
from decimal import Decimal

from .ledger import report
from .signer import number, now_ms
from .store import digest, require
from .venue import snapshot, positions


def checkpoint(venue, account):
    started = now_ms()
    mode = venue.info("userAbstraction", user=account)
    require(mode == "unifiedAccount", "equity model currently requires Unified account mode")
    observation = snapshot(venue, account)
    balances = observation["balance"]["balances"]
    cash = [b for b in balances if b["token"] == 0 and b["coin"] == "USDC"]
    require(len(cash) == 1, "missing or duplicate USDC cash balance")
    unsupported = ["non-USDC balance:" + b["coin"] for b in balances if b["token"] != 0 and number(b["total"]) != 0]
    unrealized = Decimal(0)
    for dex in observation["dexs"]:
        active = positions(dex["state"])
        if active:
            meta = venue.info("meta", **({"dex": dex["dex"]} if dex["dex"] else {}))
            if meta.get("collateralToken", 0 if not dex["dex"] else None) != 0:
                unsupported.append("non-USDC perpetual DEX:" + dex["dex"])
                continue
            unrealized += sum((number(p["unrealizedPnl"]) for p in active), Decimal(0))
    require(venue.info("userAbstraction", user=account) == mode, "account mode changed")
    return {"schema_version": 1, "kind": "unified_usdc_checkpoint", "network": venue.network, "account": account,
            "started_at_ms": started, "completed_at_ms": now_ms(), "account_mode": mode,
            "shared_usdc_balance": str(number(cash[0]["total"])), "unrealized_perp_pnl_usdc": str(unrealized),
            "marked_usdc_equity_model": str(number(cash[0]["total"])),
            "model_id": "unified_marked_balance_v1",
            "model": "shared spot USDC total is the marked collateral component; do not add unrealized P&L or per-DEX accountValue again",
            "unsupported_components": unsupported, "observation": observation,
            "limitations": ["Sequential valuation window, not an atomic account statement.",
                            "Vaults, staking, non-USDC assets and automated spot hedge accounting are outside this model.",
                            "A zero residual supports this window only, not historical completeness or mainnet authority."]}


def reconcile_equity(venue, ledger, before, after, tolerance="0.01"):
    tol = number(tolerance)
    require(tol >= 0, "negative reconciliation tolerance")
    for point in (before, after):
        require(point["schema_version"] == 1 and point["kind"] == "unified_usdc_checkpoint"
                and point["network"] == venue.network == ledger["network"]
                and point["account"] == ledger["account"] and point["account_mode"] == "unifiedAccount"
                and point["model_id"] == "unified_marked_balance_v1",
                "checkpoint/ledger scope mismatch")
    start, end = before["completed_at_ms"], after["completed_at_ms"]
    require(before["started_at_ms"] <= start < after["started_at_ms"] <= end, "overlapping or reversed checkpoints")
    # Ownership is (start, end]; adjacent reconciliations must not count one event twice.
    flows = report(venue, ledger, start + 1, end)
    balance_delta = number(after["shared_usdc_balance"]) - number(before["shared_usdc_balance"])
    mark_delta = number(after["unrealized_perp_pnl_usdc"]) - number(before["unrealized_perp_pnl_usdc"])
    net = number(flows["known_realized_net_usdc"])
    capital = number(flows["known_external_capital_usdc"])
    residual = balance_delta - capital - net - mark_delta
    gaps = list(before["unsupported_components"]) + list(after["unsupported_components"])
    if not flows["requested_window_enumerated"]: gaps.append("ledger window not enumerated")
    if flows["unclassified_ledger_types"]: gaps.append("unclassified ledger events")
    if flows["aggregated_funding_coins"]: gaps.append("aggregated funding requires independent window reconstruction")
    if any(not m["usdc_collateral_verified"] for m in flows["markets"].values()): gaps.append("spot/non-USDC cash flows")
    boundaries = []
    for kind, records in ledger["records"].items():
        for row in records.values():
            if any(p["started_at_ms"] <= row["time"] <= p["completed_at_ms"] for p in (before, after)):
                boundaries.append({"kind": kind, "time": row["time"]})
    if boundaries: gaps.append("ledger activity inside a checkpoint valuation window")
    return {"schema_version": 1, "network": venue.network, "account": ledger["account"],
            "start_exclusive_ms": start, "end_inclusive_ms": end, "tolerance_usdc": tolerance,
            "checkpoint_sha256": [digest(before), digest(after)], "ledger_sha256": digest(ledger),
            "shared_balance_change_usdc": str(balance_delta), "external_capital_usdc": str(capital),
            "observed_realized_net_usdc": str(net), "unrealized_pnl_change_usdc": str(mark_delta),
            "marked_equity_change_usdc": str(balance_delta),
            "explained_marked_change_usdc": str(capital + net + mark_delta),
            "unexplained_residual_usdc": str(residual), "within_tolerance": abs(residual) <= tol,
            "status": "incomplete" if gaps else "matched_observed_window" if abs(residual) <= tol else "unexplained_residual",
            "gaps": sorted(set(gaps)), "boundary_events": boundaries, "cash_flows": flows,
            "historical_completeness": "unproven", "mainnet_authorized": False,
            "scope": "observed Unified-USDC component model; excludes vaults, staking and non-USDC/spot cost basis"}
