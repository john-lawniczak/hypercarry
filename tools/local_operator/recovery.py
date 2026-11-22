"""Unsigned, journal-derived handoff to the bounded Rust TESTNET recovery host."""
from pathlib import Path
import uuid

from .entry import artifact, run_tool
from .signer import now_ms, validate_policy
from .store import digest, lock, private_dir, read, require
from .venue import snapshot, market, find_position, number
from .workflow import verify_plan


def prepare(venue, entry_plan, operations, limits, recovery_binary, signer_socket):
    verify_plan(entry_plan)
    p = entry_plan["policy"]
    require(entry_plan["operation"] == "open" and venue.network == p["network"] == "testnet", "owned testnet entry required")
    root = private_dir(operations)
    with lock(root / ("testnet-" + p["account"] + ".lock")):
        require(not (root / ("testnet-" + p["account"] + "-pending.json")).exists(), "reconcile pending execution before handoff")
        attempt = root / p["session_id"]
        require(read(attempt / "plan.json") == entry_plan, "entry plan differs from durable attempt")
        journal = attempt / "bracket.jsonl"
        bound = run_tool(entry_plan["tools"]["journal"], ["--journal", str(journal), "recovery-policy"], limits)
        spec = bound["policy"]["spec"]
        require(spec["plan_digest"] == digest(entry_plan), "journal does not belong to this entry")
        observation = snapshot(venue, p["account"])
        current = find_position(observation, p["coin"])
        require(current is not None and (number(current["szi"]) > 0) == p["is_buy"]
                and abs(number(current["szi"])) <= number(p["quantity"]), "unexpected recovery exposure")
        meta = market(venue, p["coin"])
        require(meta["asset"] == p["asset"], "recovery asset changed")
        ids = bound["policy"].get("repaired_exits") or {"tp": spec["tp_cloid"], "sl": spec["sl_cloid"]}
        policy = {**p, "session_id": uuid.uuid4().hex, "expires_at_ms": now_ms() + 300_000,
                  "allow_entry": False, "cancel_oids": [], "recovery_cloids": [spec["entry_cloid"], ids["tp"], ids["sl"]],
                  "close_min": limits["min_price"], "close_max": limits["max_price"],
                  "quantity": str(abs(number(current["szi"]))), "max_actions": 8, "plan_digest": "0" * 64}
        config = {"schema_version": 1, "network": "testnet", "bracket_journal": str(journal),
                  "bracket_spec_digest": bound["spec_digest"], "operations_dir": str(root), "asset_id": p["asset"],
                  "signer_address": p["signer_address"], "signer_alias": p["key_id"],
                  "signer_socket": str(Path(signer_socket).absolute()), "signer_timeout_ms": 1000,
                  "poll_ms": 15000, "session_ms": 180000, "limits": limits}
        result = {"schema_version": 1, "operation": "recovery_handoff", "policy": policy, "config": config,
                  "recovery_policy": bound["policy"], "recovery_tool": artifact(recovery_binary),
                  "observation": observation, "mainnet_authorized": False}
        policy["plan_digest"] = digest(result)
        validate_policy(policy, now_ms())
        return result
