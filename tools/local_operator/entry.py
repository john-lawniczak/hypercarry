"""Testnet bracket assembly using the existing Rust planner and lifecycle."""
from decimal import Decimal
import hashlib
import json
from pathlib import Path
import subprocess
import uuid

from .signer import now_ms, number, validate_policy
from .store import digest, lock, private_dir, read, require, write
from .venue import (coin_orders, dex_fields, find_position, market, positions,
                    fresh_action_evidence, send, snapshot)
from .release import execution_scope
from .workflow import TERMINAL, cloid, seal_plan, verify_plan


def artifact(path):
    path = Path(path).resolve(strict=True)
    return {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def run_tool(tool, args, payload=None):
    require(artifact(tool["path"]) == tool, "reviewed binary changed; prepare a new plan")
    result = subprocess.run([tool["path"], *args], input=json.dumps(payload) if payload is not None else "",
                            text=True, capture_output=True, timeout=10, check=False)
    require(result.returncode == 0, "Rust calculation/journal refused; inspect its unsigned inputs")
    require(len(result.stdout) <= 1_048_576, "oversized Rust response")
    return json.loads(result.stdout)


def live_market(venue, account, coin, observation):
    require(venue.info("userAbstraction", user=account) == "unifiedAccount", "initial local profile requires Unified account")
    metadata, contexts = venue.info("metaAndAssetCtxs", **dex_fields(coin))
    require(metadata.get("collateralToken", 0 if ":" not in coin else None) == 0, "initial profile requires USDC collateral")
    index = next(i for i, m in enumerate(metadata["universe"]) if m["name"] == coin)
    meta = market(venue, coin)
    require(meta["szDecimals"] == metadata["universe"][index]["szDecimals"], "metadata changed")
    tier_id = meta["marginTableId"]
    if 0 < tier_id < 50:
        table = {"marginTiers": [{"lowerBound": "0", "maxLeverage": tier_id}]}
    else:
        table = next((t for i, t in metadata["marginTables"] if i == tier_id), None)
        require(table is not None, "missing maintenance tiers")
    fees = venue.info("userFees", user=account)
    rate = number(fees["userCrossRate"])
    # Conservative upper bound: do not assume referral/aligned-collateral discounts.
    if ":" in coin:
        scale = number(meta["deployerFeeScale"])
        growth = meta["growthMode"]
        require(growth in ("enabled", "disabled") and 0 <= scale < 10, "unknown builder fee configuration")
        rate *= (scale + 1 if scale < 1 else scale * 2)
        if growth == "enabled":
            rate *= Decimal("0.1")
    require(observation["available_usdc"] is not None, "available collateral unknown")
    return {"asset": meta["asset"], "size_decimals": meta["szDecimals"], "max_leverage": meta["maxLeverage"],
            "mark": contexts[index]["markPx"], "available_usdc": observation["available_usdc"],
            "taker_fee_rate": str(rate), "observed_at_ms": observation["completed_at_ms"], "margin_table": table}


def target_flat(observation, coin):
    if observation["network"] == "mainnet":
        require(all(not positions(d["state"]) and not d["orders"] for d in observation["dexs"]), "mainnet pilot requires an entirely flat/clean account")
    require(find_position(observation, coin) is None and not coin_orders(observation, coin), "target market is not flat/clean")
    for dex in observation["dexs"]:
        for position in positions(dex["state"]):
            require(position["leverage"]["type"] == "isolated", "cross exposure outside initial local profile")


def exit_order(p, key, quantity, identifier=None):
    return {"a": p["asset"], "b": not p["is_buy"], "p": p[key + "_limit"], "s": str(quantity), "r": True,
            "t": {"trigger": {"isMarket": True, "triggerPx": p[key], "tpsl": key}}, "c": identifier or cloid()}


def plan_open(venue, input_value, agent, key_id, analytics_bin, journal_bin, *, mainnet_preview=False):
    require(input_value["network"] == venue.network and (venue.network == "testnet" or mainnet_preview and venue.network == "mainnet") and input_value["leverage"] == 1,
            "initial execution profile is testnet isolated 1x")
    observation = snapshot(venue, input_value["account"])
    target_flat(observation, input_value["coin"])
    tools = {"analytics": artifact(analytics_bin), "journal": artifact(journal_bin)}
    live = live_market(venue, input_value["account"], input_value["coin"], observation)
    calculated = run_tool(tools["analytics"], ["trade-calculate"], {"input": input_value, "market": live})
    require(calculated["executable"] is False, "planner unexpectedly claims authority")
    p = {"schema_version": 1, "network": venue.network, "account": input_value["account"], "signer_address": agent,
         "key_id": key_id, "session_id": uuid.uuid4().hex, "expires_at_ms": now_ms() + 300_000,
         "asset": live["asset"], "coin": input_value["coin"], "is_buy": input_value["direction"] == "long",
         "quantity": calculated["quantity"], "entry_limit": calculated["entry_limit"],
         "tp": input_value["take_profit"], "sl": input_value["stop_loss"], "tp_limit": calculated["tp_limit"],
         "sl_limit": calculated["sl_limit"], "close_min": calculated["entry_limit"], "close_max": calculated["entry_limit"],
         "leverage": 1, "allow_entry": True, "cancel_oids": [], "max_actions": 4}
    p.update(plan_digest="0" * 64, reserve_usdc=input_value["reserve_usdc"])
    validate_policy(p, now_ms(), mainnet_preview=mainnet_preview)
    orders = [{"a": p["asset"], "b": p["is_buy"], "p": p["entry_limit"], "s": p["quantity"], "r": False,
               "t": {"limit": {"tif": "Ioc"}}, "c": cloid()},
              exit_order(p, "tp", p["quantity"]), exit_order(p, "sl", p["quantity"])]
    plan = {"schema_version": 1, "operation": "open", "policy": p, "baseline": observation,
            "action": {"type": "order", "orders": orders, "grouping": "normalTpsl"}, "calculated": calculated,
            "tools": tools, "plan_expires_at_ms": p["expires_at_ms"], "fee_model": "venue-derived upper bound without discounts"}
    return seal_plan(plan)


def entry_status(venue, p, identifier):
    status = venue.info("orderStatus", user=p["account"], oid=identifier)
    if status.get("status") != "order":
        return "unknown"
    value = status["order"].get("status")
    return "terminal" if value in TERMINAL else "pending" if value == "open" else "unknown"


def terminal_without_entry(venue, plan, attempt, observation):
    """Prove an expired, never-sent or explicitly rejected entry; never infer from flatness alone."""
    intent_path = attempt / "entry-intent.json"
    if not intent_path.exists():
        return False
    intent = read(intent_path)
    p = plan["policy"]
    require(read(attempt / "plan.json") == plan and intent["action"] == plan["action"]
            and intent["network"] == p["network"] == venue.network
            and intent["key_id"] == p["key_id"] and intent["signer_address"] == p["signer_address"],
            "entry rejection evidence differs from approved attempt")
    if now_ms() <= intent["expires_after"]:
        return False
    broadcast = attempt / "entry-broadcast.json"
    response = attempt / "entry-response.json"
    if broadcast.exists():
        require(read(broadcast)["nonce"] == intent["nonce"], "entry broadcast nonce mismatch")
        if not response.exists() or read(response) != {"status": "err", "response": "Action already expired"}:
            return False
        source = "venue_expiry_rejection"
    else:
        # send() durably writes the broadcast marker before any exchange call.
        if response.exists():
            return False
        source = "never_broadcast"
    if find_position(observation, p["coin"]) or coin_orders(observation, p["coin"]):
        return False
    statuses = [venue.info("orderStatus", user=p["account"], oid=o["c"]) for o in plan["action"]["orders"]]
    if any(s != {"status": "unknownOid"} for s in statuses):
        return False
    write(attempt / ("entry-rejection-" + uuid.uuid4().hex + ".json"),
          {"source": source, "plan_digest": digest(plan), "nonce": intent["nonce"],
           "expires_after": intent["expires_after"], "observed_at_ms": now_ms(),
           "snapshot_completed_at_ms": observation["completed_at_ms"], "order_statuses": statuses})
    return True


def protection(observation, p, quantity, eligible_ids=None):
    found = {}
    for key, kind in (("tp", "Take Profit Market"), ("sl", "Stop Market")):
        found[key] = next((o for o in coin_orders(observation, p["coin"])
                           if (eligible_ids is None or o.get("cloid") in eligible_ids)
                           and o.get("orderType") == kind and o.get("isTrigger") is True
                           and o.get("reduceOnly") is True and o.get("side") == ("A" if p["is_buy"] else "B")
                           and number(o["sz"]) == quantity and number(o["triggerPx"]) == number(p[key])
                           and number(o["limitPx"]) == number(p[key + "_limit"])
                           and not o.get("children")), None)
    return found


def reserve_bracket(plan, attempt, nonce):
    p = plan["policy"]
    ids = [o["c"] for o in plan["action"]["orders"]]
    spec = {"trade_id": p["session_id"], "plan_digest": digest(plan), "network": p["network"], "account": p["account"],
            "coin": p["coin"], "entry_side": "buy" if p["is_buy"] else "sell", "quantity": p["quantity"],
            "tp": p["tp"], "sl": p["sl"], "entry_cloid": ids[0], "tp_cloid": ids[1], "sl_cloid": ids[2]}
    run_tool(plan["tools"]["journal"], ["--journal", str(attempt / "bracket.jsonl"), *(["--allow-mainnet-journal"] if p["network"] == "mainnet" else []), "reserve"], {"spec": spec, "nonce": nonce})


def observe_bracket(plan, attempt, observation, outcome, owned_ids):
    p = plan["policy"]
    current = find_position(observation, p["coin"])
    quantity = abs(number(current["szi"])) if current else Decimal(0)
    exits = protection(observation, p, quantity)
    fields = {}
    for key in ("tp", "sl"):
        order = exits[key]
        fields[key] = None if order is None else {
            "cloid": order["cloid"], "coin": p["coin"],
            "side": "sell" if p["is_buy"] else "buy", "quantity": order["sz"], "trigger": order["triggerPx"],
            "reduce_only": True, "is_trigger": True, "is_take_profit": key == "tp"}
    data = {"account": p["account"], "network": p["network"], "coin": p["coin"], "entry_outcome": outcome,
            "signed_position": current["szi"] if current else "0", **fields,
            "unmanaged_orders": sum(o.get("cloid") not in owned_ids for o in coin_orders(observation, p["coin"])),
            "started_at_ms": observation["started_at_ms"], "completed_at_ms": observation["completed_at_ms"], "complete": True}
    return run_tool(plan["tools"]["journal"], ["--journal", str(attempt / "bracket.jsonl"), "observe"],
                    {"kind": "observed", "observation": data})


def final_entry_evidence(venue, plan):
    p, original = plan["policy"], plan["calculated"]
    fresh_action_evidence(venue, p, plan["action"])
    fresh_snapshot = snapshot(venue, p["account"])
    target_flat(fresh_snapshot, p["coin"])
    latest = live_market(venue, p["account"], p["coin"], fresh_snapshot)
    checked = run_tool(plan["tools"]["analytics"], ["trade-calculate"], {"input": original["input"], "market": latest})
    require(number(checked["quantity"]) >= number(p["quantity"])
            and number(latest["taker_fee_rate"]) <= number(original["market"]["taker_fee_rate"]),
            "risk/fee inputs changed during signing")
    settings = venue.info("activeAssetData", user=p["account"], coin=p["coin"])
    require(settings["leverage"]["type"] == "isolated" and settings["leverage"]["value"] == 1,
            "margin configuration changed during signing")
    require(settings["user"].lower() == p["account"] and settings["coin"] == p["coin"]
            and isinstance(settings["maxTradeSzs"], list) and len(settings["maxTradeSzs"]) == 2,
            "active asset capacity identity/schema changed")
    require(number(settings["maxTradeSzs"][0 if p["is_buy"] else 1]) >= number(p["quantity"]),
            "venue maximum trade size is insufficient")


def record_owned_position(plan, attempt, observation):
    p = plan["policy"]
    path = attempt / "position.json"
    if path.exists():
        require(read(path)["entry_plan_digest"] == digest(plan), "owned record differs from entry")
        return
    write(attempt / "position.json", {"schema_version": 1, "origin": "owned_bracket_entry", "network": p["network"],
          "account": p["account"], "coin": p["coin"], "adopted_at_ms": observation["completed_at_ms"],
          "initial_position": find_position(observation, p["coin"]), "owned_exits": coin_orders(observation, p["coin"]),
          "history_complete": False, "observation": observation, "entry_plan_digest": digest(plan)})


def execute_open(venue, plan, socket_path, state_dir):
    verify_plan(plan)
    p = plan["policy"]
    require(plan["operation"] == "open", "not an entry plan")
    execution_scope(venue, p, plan, state_dir)
    require(now_ms() < plan["plan_expires_at_ms"], "plan expired")
    root = private_dir(state_dir)
    with lock(root / (p["network"] + "-" + p["account"] + ".lock")):
        require(not (root / (p["network"] + "-" + p["account"] + ".entries-disabled")).exists(),
                "recovery has frozen account entries; explicit recovery review required")
        pending = root / (p["network"] + "-" + p["account"] + "-pending.json")
        require(not pending.exists(), "account has unresolved attempt")
        attempt = private_dir(root / p["session_id"])
        require(not (attempt / "plan.json").exists(), "entry attempt already reserved; never replay")
        current = snapshot(venue, p["account"])
        target_flat(current, p["coin"])
        live = live_market(venue, p["account"], p["coin"], current)
        original = plan["calculated"]
        require(live["asset"] == p["asset"] and live["size_decimals"] == original["market"]["size_decimals"], "market changed")
        # Recalculate all risk at the fresh quote; retain exact approved size/price.
        fresh = run_tool(plan["tools"]["analytics"], ["trade-calculate"], {"input": original["input"], "market": live})
        require(number(fresh["quantity"]) >= number(p["quantity"]), "fresh reserve/risk supports less size")
        require(number(live["taker_fee_rate"]) <= number(original["market"]["taker_fee_rate"]), "fee ceiling increased")
        # Check executable depth at the immutable approved limit. Reapplying the
        # slippage percentage to a new quote would reject valid in-bound movement.
        fresh_action_evidence(venue, p, plan["action"])
        require(artifact(plan["tools"]["journal"]["path"]) == plan["tools"]["journal"], "journal binary changed")
        write(attempt / "plan.json", plan)
        write(pending, {"session_id": p["session_id"], "plan_digest": digest(plan)})
        # Apply isolated 1x before the entry. An uncertain response blocks entry.
        configured = send(venue, p, {"type": "updateLeverage", "asset": p["asset"], "isCross": False, "leverage": 1},
                          socket_path, attempt, "configure")
        require(configured.get("status") == "ok", "margin configuration not acknowledged")
        configuration = venue.info("activeAssetData", user=p["account"], coin=p["coin"])
        require(configuration["leverage"]["type"] == "isolated" and configuration["leverage"]["value"] == 1,
                "isolated 1x was not verified")
        # The configuration acknowledgement and target-flat recheck are separate evidence.
        target_flat(snapshot(venue, p["account"]), p["coin"])
        send(venue, p, plan["action"], socket_path, attempt, "entry",
             before_sign=lambda nonce: reserve_bracket(plan, attempt, nonce), before_submit=lambda: final_entry_evidence(venue, plan))
        outcome = entry_status(venue, p, plan["action"]["orders"][0]["c"])
        observation = snapshot(venue, p["account"])
        if outcome == "unknown" and terminal_without_entry(venue, plan, attempt, observation):
            outcome = "terminal"
        owned = {o["c"] for o in plan["action"]["orders"]}
        position = find_position(observation, p["coin"])
        if outcome == "terminal" and position:
            quantity = abs(number(position["szi"]))
            require((number(position["szi"]) > 0) == p["is_buy"] and quantity <= number(p["quantity"])
                    and position["leverage"]["type"] == "isolated" and position["leverage"]["value"] == 1,
                    "unexpected filled position")
            require(all(o.get("cloid") in owned for o in coin_orders(observation, p["coin"])), "foreign target order")
            exits = protection(observation, p, quantity)
            repair = [exit_order(p, key, quantity) for key in ("tp", "sl") if exits[key] is None]
            if repair:
                observe_bracket(plan, attempt, observation, outcome, owned)
                reservations = {o["t"]["trigger"]["tpsl"]: o["c"] for o in repair}
                run_tool(plan["tools"]["journal"], ["--journal", str(attempt / "bracket.jsonl"), "observe"],
                         {"kind": "protection_repair_reserved", "take_profit_cloid": reservations.get("tp"),
                          "stop_loss_cloid": reservations.get("sl")})
                owned.update(o["c"] for o in repair)
                send(venue, p, {"type": "order", "orders": repair, "grouping": "na"}, socket_path, attempt, "repair")
                observation = snapshot(venue, p["account"])
        lifecycle = observe_bracket(plan, attempt, observation, outcome, owned)
        result = {"status": lifecycle["state"], "lifecycle": lifecycle, "observation": observation}
        if position and (observation["available_usdc"] is None or
                         number(observation["available_usdc"]) < number(original["input"]["reserve_usdc"])):
            result["status"] = "reserve_breached"
        write(attempt / "entry-observation.json", result)
        if result["status"] in ("open", "closed"):
            if lifecycle["state"] == "open":
                record_owned_position(plan, attempt, observation)
            pending.unlink()
        return result


def reconcile_open(venue, plan, state_dir):
    verify_plan(plan)
    p = plan["policy"]
    require(venue.network == p["network"], "reconciliation network mismatch")
    root = private_dir(state_dir)
    with lock(root / (p["network"] + "-" + p["account"] + ".lock")):
        attempt = root / p["session_id"]
        pending = root / (p["network"] + "-" + p["account"] + "-pending.json")
        require(read(pending) == {"session_id": p["session_id"], "plan_digest": digest(plan)}, "pending entry mismatch")
        owned = {o["c"] for o in plan["action"]["orders"]}
        if (attempt / "repair-intent.json").exists():
            owned.update(o["c"] for o in read(attempt / "repair-intent.json")["action"]["orders"])
        observation = snapshot(venue, p["account"])
        if not (attempt / "entry-intent.json").exists():
            target_flat(observation, p["coin"])
            if (attempt / "configure-intent.json").exists():
                require(now_ms() > read(attempt / "configure-intent.json")["expires_after"], "wait for configuration signature expiry")
            result = {"status": "no_entry_attempted", "observation": observation}
            write(attempt / ("reconcile-" + uuid.uuid4().hex + ".json"), result)
            pending.unlink()
            return result
        outcome = entry_status(venue, p, plan["action"]["orders"][0]["c"])
        if outcome == "unknown" and terminal_without_entry(venue, plan, attempt, observation):
            outcome = "terminal"
        lifecycle = observe_bracket(plan, attempt, observation, outcome, owned)
        result = {"status": lifecycle["state"], "lifecycle": lifecycle, "observation": observation}
        if find_position(observation, p["coin"]) and (observation["available_usdc"] is None or
                                                     number(observation["available_usdc"]) < number(p["reserve_usdc"])):
            result["status"] = "reserve_breached"
        write(attempt / ("reconcile-" + uuid.uuid4().hex + ".json"), result)
        if result["status"] in ("open", "closed"):
            if result["status"] == "open":
                record_owned_position(plan, attempt, observation)
            pending.unlink()
        return result
