#!/usr/bin/env python3
"""Prepare and locally unlock one bounded TESTNET plan; never submit an order.

Run from the repository root with the pinned SDK. Password entry stays in the
user's terminal. A second terminal/operator explicitly executes the saved plan.
"""
import argparse
import json
from pathlib import Path
import shlex
import subprocess
import sys
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from tools.local_operator.entry import plan_open
from tools.local_operator.signer import address, now_ms, number, serve, validate_policy
from tools.local_operator.store import private_dir, read, require
from tools.local_operator.venue import Venue, market, price_round
from tools.local_operator.workflow import plan_close, save_plan, seal_plan


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for arg in ("account", "agent", "keystore", "directory"):
        parser.add_argument("--" + arg, required=True)
    parser.add_argument("--coin", default="BTC")
    parser.add_argument("--notional-usdc", default="15")
    parser.add_argument("--reserve-usdc", default="950")
    parser.add_argument("--close-adoption", help="owned position.json; prepares full close instead of entry")
    parser.add_argument("--quantity", help="optional partial close quantity")
    parser.add_argument("--serve", action="store_true", help="show policy and prompt locally to unlock signer")
    parser.add_argument("--candidate", help="optional frozen source/binary manifest for qualifying sessions")
    parser.add_argument("--candidate-sha256")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    require(Path.cwd().resolve() == repo, "run from repository root")
    require(not subprocess.check_output(["git", "status", "--porcelain"], text=True).strip(),
            "attended session requires a clean committed source tree")
    require(bool(args.candidate) == bool(args.candidate_sha256), "candidate path and trusted hash must be supplied together")
    frozen = None
    if args.candidate:
        from tools.local_operator.release import verify_candidate
        from tools.local_operator.store import digest
        frozen = read(args.candidate)
        require(digest(frozen) == args.candidate_sha256, "candidate digest mismatch")
        verify_candidate(frozen, repo)
    account, agent = address(args.account.lower()), address(args.agent.lower())
    require(account != agent, "dedicated testnet agent required")
    root = private_dir(Path(args.directory).absolute())
    sessions = private_dir(root / "plans")
    signer = private_dir(root / "signer")
    operations = private_dir(root / "operations")
    socket = signer / "agent.sock"
    require(len(str(socket).encode()) < 100, "use a short private directory for Unix sockets")
    venue = Venue("testnet")
    key_id = "foundry/hypercarry-testnet-agent"
    if args.close_adoption:
        record = read(args.close_adoption)
        require(record["network"] == "testnet" and record["account"] == account, "wrong close account/network")
        plan = plan_close(venue, record, agent, key_id, args.quantity, 30)
    else:
        require(args.quantity is None and 12 <= number(args.notional_usdc) <= 20
                and number(args.reserve_usdc) >= 950, "attended pilot requires $12-$20 notional and at least $950 reserve")
        meta = market(venue, args.coin)
        book = venue.info("l2Book", coin=args.coin)
        mark = number(book["levels"][1][0]["px"])
        value = {"network": "testnet", "account": account, "coin": args.coin, "direction": "long",
                 "notional_budget": args.notional_usdc, "leverage": 1, "reserve_usdc": args.reserve_usdc,
                 "entry_slippage_percent": "0.5", "exit_slippage_percent": "10",
                 "take_profit": str(price_round(mark * number("1.03"), meta["szDecimals"], False)),
                 "stop_loss": str(price_round(mark * number("0.99"), meta["szDecimals"], True)),
                 "min_health_at_stop": "2"}
        plan = plan_open(venue, value, agent, key_id,
                         frozen["binaries"]["analytics"]["path"] if frozen else repo / "target/debug/hypercarry",
                         frozen["binaries"]["journal"]["path"] if frozen else repo / "target/debug/hypercarry-bracket-journal")
    if frozen:
        if "tools" in plan:
            require(plan["tools"] == {k: frozen["binaries"][k] for k in ("analytics", "journal")}, "plan binaries differ from frozen candidate")
        plan["candidate_sha256"] = args.candidate_sha256
    # Explicitly displayed and hashed approval; existing policies retain ten seconds.
    plan["policy"]["action_ttl_ms"] = 30_000
    validate_policy(plan["policy"], now_ms())
    seal_plan(plan)
    path = sessions / (plan["policy"]["session_id"] + ".json")
    saved = save_plan(path, plan)
    command = ["uv", "run", "--offline", "--no-project", "--with", "hyperliquid-python-sdk==0.24.0",
               "python", "-m", "tools.local_operator", "execute", "--plan", str(path),
               "--socket", str(socket), "--state", str(operations), "--acknowledge-testnet"]
    print(json.dumps({**saved, "source_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                      "policy": plan["policy"], "execute_in_second_terminal": shlex.join(command)}, indent=2), flush=True)
    print("Prepared TESTNET plan only. No order submitted. Plan expires in five minutes.", flush=True)
    if args.serve:
        serve(path.with_suffix(".policy.json"), args.keystore, signer, socket)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
    except Exception as error:
        print("attended preparation failed (" + type(error).__name__ + "); no automatic retry", file=sys.stderr)
        sys.exit(1)
