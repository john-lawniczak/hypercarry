"""Explicit testnet exit amendment: verify replacement before retiring old exits."""
from decimal import Decimal
import uuid

from .entry import exit_order, protection
from .signer import now_ms, number, validate_policy
from .store import digest, lock, private_dir, read, require, write
from .venue import coin_orders, find_position, market, price_round, send, snapshot
from .workflow import policy_for, seal_plan, validate_adopted, verify_plan


def plan_protect(venue, record, agent, key_id, take_profit, stop_loss, exit_slippage_percent="10"):
    require(venue.network == record["network"] == "testnet", "exit amendment is testnet-only")
    observation = snapshot(venue, record["account"])
    position = validate_adopted(record, observation)
    require(position is not None, "no exposure to protect")
    quantity = abs(number(position["szi"]))
    mark = number(position["positionValue"]) / quantity
    meta = market(venue, record["coin"])
    p = policy_for(record, position, meta, agent, key_id, quantity, mark)
    tp, sl = number(take_profit), number(stop_loss)
    tolerance = number(exit_slippage_percent) / 100
    require(0 < tolerance <= Decimal("0.1"), "exit tolerance must be in (0,10] percent")
    require((sl < mark < tp) if p["is_buy"] else (tp < mark < sl), "triggers must bracket current mark")
    for value in (tp, sl):
        require(price_round(value, meta["szDecimals"], False) == value, "invalid trigger precision")
    old_stops = [number(o["triggerPx"]) for o in record["owned_exits"] if o["orderType"] == "Stop Market"]
    if old_stops:
        require(sl >= max(old_stops) if p["is_buy"] else sl <= min(old_stops), "this profile permits tightening, not loosening, existing stops")
    factor = 1 - tolerance if p["is_buy"] else 1 + tolerance
    p.update(tp=str(tp), sl=str(sl), tp_limit=str(price_round(tp * factor, meta["szDecimals"], p["is_buy"])),
             sl_limit=str(price_round(sl * factor, meta["szDecimals"], p["is_buy"])))
    liquidation = number(position["liquidationPx"])
    require(number(p["sl_limit"]) > liquidation if p["is_buy"] else number(p["sl_limit"]) < liquidation,
            "stop execution bound crosses observed liquidation price")
    validate_policy(p, now_ms())
    return seal_plan({"schema_version": 1, "operation": "protect", "policy": p, "adoption": record, "baseline": observation,
            "action": {"type": "order", "orders": [exit_order(p, "tp", quantity), exit_order(p, "sl", quantity)], "grouping": "na"},
            "plan_expires_at_ms": p["expires_at_ms"]})


def execute_protect(venue, plan, socket_path, state_dir):
    verify_plan(plan)
    p = plan["policy"]
    require(venue.network == p["network"] == "testnet" and plan["operation"] == "protect", "not a testnet protection plan")
    require(now_ms() < plan["plan_expires_at_ms"], "plan expired")
    root = private_dir(state_dir)
    with lock(root / ("testnet-" + p["account"] + ".lock")):
        pending = root / ("testnet-" + p["account"] + "-pending.json")
        require(not pending.exists(), "account has unresolved attempt")
        attempt = private_dir(root / p["session_id"])
        require(not (attempt / "plan.json").exists(), "protection attempt already reserved")
        observation = snapshot(venue, p["account"])
        position = validate_adopted(plan["adoption"], observation)
        require(position and abs(number(position["szi"])) == number(p["quantity"]), "position resized")
        write(attempt / "plan.json", plan)
        write(pending, {"session_id": p["session_id"], "plan_digest": digest(plan)})
        send(venue, p, plan["action"], socket_path, attempt, "protect")
        new_ids = {o["c"] for o in plan["action"]["orders"]}
        old_ids = set(p["cancel_oids"])
        for i in range(0, len(p["cancel_oids"]), 3):
            def verify_replacements():
                current = snapshot(venue, p["account"])
                actual = find_position(current, p["coin"])
                require(actual and actual["szi"] == position["szi"], "position changed; preserve old protection")
                exits = protection(current, p, number(p["quantity"]), new_ids)
                require(all(o is not None and o.get("cloid") in new_ids for o in exits.values()), "replacement exits not verified; preserve old protection")
                require(all(o["oid"] in old_ids or o.get("cloid") in new_ids for o in coin_orders(current, p["coin"])), "foreign order; stop amendment")
            verify_replacements()
            send(venue, p, {"type": "cancel", "cancels": [{"a": p["asset"], "o": oid} for oid in p["cancel_oids"][i:i+3]]},
                 socket_path, attempt, "retire-" + str(i), before_submit=verify_replacements)
        current = snapshot(venue, p["account"])
        exits = protection(current, p, number(p["quantity"]), new_ids)
        actual = find_position(current, p["coin"])
        verified = bool(actual and actual["szi"] == position["szi"] and all(o is not None and o.get("cloid") in new_ids for o in exits.values())
                        and all(o.get("cloid") in new_ids for o in coin_orders(current, p["coin"])))
        result = {"status": "protection_verified" if verified else "reconciliation_required", "observation": current}
        write(attempt / "protection-observation.json", result)
        if verified:
            record = {**plan["adoption"], "initial_position": actual, "owned_exits": coin_orders(current, p["coin"]),
                      "observation": current, "amended_at_ms": now_ms()}
            write(attempt / "position.json", record)
            pending.unlink()
        return result


def reconcile_protect(venue, plan, state_dir):
    verify_plan(plan)
    p = plan["policy"]
    root = private_dir(state_dir)
    with lock(root / ("testnet-" + p["account"] + ".lock")):
        pending = root / ("testnet-" + p["account"] + "-pending.json")
        require(read(pending) == {"session_id": p["session_id"], "plan_digest": digest(plan)}, "pending amendment mismatch")
        current = snapshot(venue, p["account"])
        actual = find_position(current, p["coin"])
        new_ids = {o["c"] for o in plan["action"]["orders"]}
        exits = protection(current, p, number(p["quantity"]), new_ids)
        orders = coin_orders(current, p["coin"])
        verified = bool(actual and abs(number(actual["szi"])) == number(p["quantity"])
                        and (number(actual["szi"]) > 0) == p["is_buy"]
                        and all(exits.values()) and all(o.get("cloid") in new_ids for o in orders))
        result = {"status": "protection_verified" if verified else "reconciliation_required", "observation": current}
        write(root / p["session_id"] / ("reconcile-" + uuid.uuid4().hex + ".json"), result)
        if verified:
            pending.unlink()
        return result
