"""Explicit adoption and journaled close workflow, without fabricated entries."""
from decimal import Decimal
import copy
from pathlib import Path
import uuid

from .signer import now_ms, number, validate_policy
from .release import execution_scope
from .store import digest, lock, private_dir, read, require, write
from .venue import (check_close_notional, close_quote, coin_orders, find_position, fresh_action_evidence, market, send, snapshot)


def cloid():
    return "0x" + uuid.uuid4().hex


def seal_plan(plan):
    plan["policy"]["plan_digest"] = "0" * 64
    plan["policy"]["plan_digest"] = digest(plan)
    return plan


def verify_plan(plan):
    candidate = copy.deepcopy(plan)
    expected = candidate["policy"]["plan_digest"]
    candidate["policy"]["plan_digest"] = "0" * 64
    require(digest(candidate) == expected, "full plan changed after policy approval")


def closing_orders(observation, position):
    side = "B" if number(position["szi"]) < 0 else "A"
    result = []
    for order in coin_orders(observation, position["coin"]):
        require(order.get("reduceOnly") is True and order.get("isTrigger") is True
                and order["side"] == side and order.get("orderType") in ("Take Profit Market", "Stop Market")
                and not order.get("children"), "unmanaged target-market orders; reconcile before adoption")
        result.append(order)
    return result


def adoption(venue, account, coin):
    observation = snapshot(venue, account)
    position = find_position(observation, coin)
    require(position is not None and position["leverage"]["type"] == "isolated"
            and position["leverage"]["value"] == 1, "adoption requires an isolated 1x position")
    owned = closing_orders(observation, position)
    return {"schema_version": 1, "origin": "adopted_existing_position", "network": venue.network,
            "account": account, "coin": coin, "adopted_at_ms": observation["completed_at_ms"],
            "initial_position": position, "owned_exits": owned,
            "history_complete": False, "observation": observation}


def validate_adopted(record, observation):
    require(record["network"] == observation["network"] and record["account"] == observation["account"],
            "adoption identity mismatch")
    position = find_position(observation, record["coin"])
    old = record["initial_position"]
    if position is not None:
        quantity, initial = number(position["szi"]), number(old["szi"])
        require(quantity * initial > 0 and abs(quantity) <= abs(initial)
                and position["entryPx"] == old["entryPx"]
                and position["leverage"]["type"] == old["leverage"]["type"] == "isolated"
                and position["leverage"]["value"] == old["leverage"]["value"] == 1,
                "position changed direction, increased, changed entry or margin configuration")
    allowed = {o["oid"]: o for o in record["owned_exits"]}
    for order in coin_orders(observation, record["coin"]):
        require(order["oid"] in allowed, "unowned target order")
        previous = allowed[order["oid"]]
        require(all(order.get(k) == previous.get(k) for k in ("coin", "side", "sz", "limitPx", "triggerPx", "reduceOnly", "orderType")),
                "owned exit was changed outside this workflow")
    return position


def policy_for(record, position, meta, agent, key_id, quantity, close_limit, *, mainnet_preview=False):
    # Close-only sessions still bind explicit positive exit values; no protection
    # is created by the close command. Later amendment uses its own approved plan.
    entry = number(position["entryPx"])
    is_buy = number(position["szi"]) > 0
    exits = {"tp" if o["orderType"] == "Take Profit Market" else "sl": o for o in record["owned_exits"]}
    p = {"schema_version": 1, "network": record["network"], "account": record["account"],
         "signer_address": agent, "key_id": key_id, "session_id": uuid.uuid4().hex,
         "expires_at_ms": now_ms() + 300_000, "asset": meta["asset"], "coin": record["coin"],
         "is_buy": is_buy, "quantity": str(quantity), "entry_limit": str(entry),
         "close_min": str(close_limit), "close_max": str(close_limit), "leverage": 1,
         "allow_entry": False, "cancel_oids": [o["oid"] for o in record["owned_exits"]], "max_actions": 5}
    p.update(plan_digest="0" * 64, reserve_usdc="0")
    for key, factor in (("tp", Decimal("1.2") if is_buy else Decimal("0.8")),
                        ("sl", Decimal("0.9") if is_buy else Decimal("1.1"))):
        p[key] = exits[key]["triggerPx"] if key in exits else str(entry * factor)
        p[key + "_limit"] = exits[key]["limitPx"] if key in exits else p[key]
    validate_policy(p, now_ms(), mainnet_preview=mainnet_preview)
    return p


def plan_close(venue, record, agent, key_id, quantity=None, slippage_bps=30, *, mainnet_preview=False):
    require(venue.network == record["network"] and (venue.network == "testnet" or mainnet_preview and venue.network == "mainnet"), "mainnet close planning needs explicit preview")
    observation = snapshot(venue, record["account"])
    position = validate_adopted(record, observation)
    require(position is not None, "position is already flat")
    residual = abs(number(position["szi"]))
    quantity = residual if quantity is None else number(quantity)
    require(0 < quantity <= residual, "close quantity exceeds current position")
    meta = market(venue, record["coin"])
    step = Decimal(1).scaleb(-meta["szDecimals"])
    require(quantity % step == 0, "unquantized close size")
    limit = close_quote(venue, record["coin"], number(position["szi"]) < 0, quantity, meta["szDecimals"], slippage_bps)
    check_close_notional(quantity, limit, residual)
    p = policy_for(record, position, meta, agent, key_id, quantity, limit, mainnet_preview=mainnet_preview)
    action = {"type": "order", "orders": [{"a": meta["asset"], "b": not p["is_buy"],
              "p": str(limit), "s": str(quantity), "r": True, "t": {"limit": {"tif": "Ioc"}}, "c": cloid()}], "grouping": "na"}
    return seal_plan({"schema_version": 1, "operation": "close", "policy": p, "adoption": record,
            "baseline": observation, "action": action, "size_decimals": meta["szDecimals"],
            "slippage_bps": slippage_bps, "plan_expires_at_ms": p["expires_at_ms"]})


def save_plan(path, plan):
    verify_plan(plan)
    path = Path(path)
    write(path, plan)
    write(path.with_suffix(".policy.json"), plan["policy"])
    return {"plan": str(path), "policy": str(path.with_suffix(".policy.json")), "sha256": digest(plan)}


TERMINAL = {"filled", "canceled", "rejected", "marginCanceled", "vaultWithdrawalCanceled",
            "openInterestCapCanceled", "selfTradeCanceled", "reduceOnlyCanceled", "siblingFilledCanceled",
            "delistedCanceled", "liquidatedCanceled", "scheduledCancel", "tickRejected", "minTradeNtlRejected",
            "perpMarginRejected", "reduceOnlyRejected", "badAloPxRejected", "iocCancelRejected",
            "badTriggerPxRejected", "marketOrderNoLiquidityRejected", "openInterestIncreaseRejected",
            "oracleRejected", "perpMaxPositionRejected", "positionIncreaseAtOpenInterestCapRejected",
            "positionFlipAtOpenInterestCapRejected", "tooAggressiveAtOpenInterestCapRejected"}


def reconcile(venue, plan):
    p = plan["policy"]
    require(venue.network == p["network"], "reconciliation network mismatch")
    statuses = []
    # A missing order is uncertainty, never proof that the request was not sent.
    for order in plan["action"].get("orders", []):
        result = venue.info("orderStatus", user=p["account"], oid=order["c"])
        statuses.append(result)
    observation = snapshot(venue, p["account"])
    position = find_position(observation, p["coin"])
    remaining = number(position["szi"]) if position else Decimal(0)
    terminal = all(s.get("status") == "order" and s["order"].get("status") in TERMINAL for s in statuses)
    orders = coin_orders(observation, p["coin"])
    status = "reconciliation_required"
    if terminal:
        status = "closed" if remaining == 0 and not orders else "flat_cleanup_required" if remaining == 0 else "residual_position"
    return {"status": status, "remaining": str(remaining), "orders": orders,
            "order_statuses": statuses, "observation": observation}


def execute_close(venue, plan, socket_path, state_dir):
    verify_plan(plan)
    p = plan["policy"]
    require(plan["operation"] == "close", "not a close plan")
    execution_scope(venue, p, plan, state_dir)
    require(now_ms() < plan["plan_expires_at_ms"], "plan expired; prepare a fresh plan")
    root = private_dir(state_dir)
    with lock(root / (p["network"] + "-" + p["account"] + ".lock")):
        pending = root / (p["network"] + "-" + p["account"] + "-pending.json")
        require(not pending.exists(), "account has an unresolved attempt; reconcile it before another plan")
        attempt = private_dir(root / p["session_id"])
        require(not (attempt / "plan.json").exists(), "attempt already reserved; reconcile without replaying")
        current = snapshot(venue, p["account"])
        position = validate_adopted(plan["adoption"], current)
        baseline = find_position(plan["baseline"], p["coin"])
        require(position is not None and position["szi"] == baseline["szi"], "position resized since approval")
        meta = market(venue, p["coin"])
        require(meta["asset"] == p["asset"] and meta["szDecimals"] == plan["size_decimals"], "market metadata changed")
        # Preserve the exact approved limit; do not apply slippage a second time
        # to a moving quote. Final post-sign checks repeat depth and residual checks.
        fresh_action_evidence(venue, p, plan["action"])
        write(attempt / "plan.json", plan)
        write(pending, {"session_id": p["session_id"], "plan_digest": digest(plan)})
        def final_close_check():
            fresh = snapshot(venue, p["account"])
            actual = validate_adopted(plan["adoption"], fresh)
            require(actual and actual["szi"] == baseline["szi"], "position changed during signing")
            fresh_action_evidence(venue, p, plan["action"])
        send(venue, p, plan["action"], socket_path, attempt, "close", before_submit=final_close_check)
        result = reconcile(venue, plan)
        write(attempt / "close-observation.json", result)
        if result["status"] == "flat_cleanup_required":
            owned = set(p["cancel_oids"])
            require(all(o["oid"] in owned for o in result["orders"]), "unowned orphan order; no cleanup")
            for i in range(0, len(result["orders"]), 3):
                # Recheck flatness before each cancellation, preserving protection if exposure returns.
                fresh = snapshot(venue, p["account"])
                require(find_position(fresh, p["coin"]) is None, "position reopened; retain exits")
                action = {"type": "cancel", "cancels": [{"a": p["asset"], "o": o["oid"]}
                          for o in result["orders"][i:i+3]]}
                send(venue, p, action, socket_path, attempt, "cleanup-" + str(i))
            result = reconcile(venue, plan)
            write(attempt / "final-observation.json", result)
        if result["status"] in ("closed", "residual_position"):
            validate_adopted(plan["adoption"], result["observation"])
            pending.unlink()
        return result


def reconcile_pending(venue, plan, state_dir):
    """Release an account reservation only from fresh terminal venue evidence."""
    verify_plan(plan)
    p = plan["policy"]
    root = private_dir(state_dir)
    with lock(root / (p["network"] + "-" + p["account"] + ".lock")):
        pending = root / (p["network"] + "-" + p["account"] + "-pending.json")
        require(read(pending) == {"session_id": p["session_id"], "plan_digest": digest(plan)}, "pending plan mismatch")
        result = reconcile(venue, plan)
        validate_adopted(plan["adoption"], result["observation"])
        write(root / p["session_id"] / ("reconcile-" + uuid.uuid4().hex + ".json"), result)
        if result["status"] in ("closed", "residual_position"):
            pending.unlink()
        return result
