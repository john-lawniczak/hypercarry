"""Local operator entry point. Execution commands remain testnet-only."""
import argparse
import json
import sys
from .signer import serve
from .store import digest, read, require, write
from .venue import Venue, snapshot
from .workflow import adoption, execute_close, plan_close, reconcile, reconcile_pending, save_plan
from .entry import execute_open, plan_open, reconcile_open
from .protection import execute_protect, plan_protect, reconcile_protect
from .ledger import report, sync
from .monitor import cached, watch
from .signer import now_ms
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    signer = commands.add_parser("signer", help="approve and unlock one bounded testnet agent session")
    for option in ("policy", "keystore", "state", "socket"):
        signer.add_argument("--" + option, required=True)
    status = commands.add_parser("status", help="unsigned all-DEX portfolio snapshot")
    status.add_argument("--network", choices=("mainnet", "testnet"), required=True)
    status.add_argument("--account", required=True)
    status.add_argument("--cache-dir", help="read collector cache with explicit age instead of querying")
    status.add_argument("--max-age-ms", type=int, default=30000)
    adopt = commands.add_parser("adopt", help="explicitly record existing exposure and owned exits")
    adopt.add_argument("--network", choices=("mainnet", "testnet"), required=True)
    for option in ("account", "coin", "output"):
        adopt.add_argument("--" + option, required=True)
    close = commands.add_parser("plan-close", help="prepare an unsigned full or partial testnet close")
    for option in ("adoption", "agent", "key-id", "output"):
        close.add_argument("--" + option, required=True)
    close.add_argument("--quantity")
    close.add_argument("--slippage-bps", type=int, default=30)
    entry = commands.add_parser("plan-open", help="calculate a testnet bracket with the Rust planner")
    for option in ("input", "agent", "key-id", "analytics-bin", "journal-bin", "output"):
        entry.add_argument("--" + option, required=True)
    protect = commands.add_parser("plan-protect", help="replace TP/SL while retaining old exits until verified")
    for option in ("adoption", "agent", "key-id", "take-profit", "stop-loss", "output"):
        protect.add_argument("--" + option, required=True)
    protect.add_argument("--exit-slippage-percent", default="10")
    execute = commands.add_parser("execute", help="execute one locally approved testnet plan")
    for option in ("plan", "socket", "state"):
        execute.add_argument("--" + option, required=True)
    execute.add_argument("--acknowledge-testnet", action="store_true", required=True)
    check = commands.add_parser("reconcile", help="read actual order outcomes without retrying writes")
    check.add_argument("--plan", required=True)
    check.add_argument("--state", help="also reconcile the durable account reservation")
    ledger_sync = commands.add_parser("ledger-sync", help="ingest overlapping fills/funding/transfers privately")
    for option in ("network", "account", "directory"):
        ledger_sync.add_argument("--" + option, required=True)
    ledger_sync.add_argument("--start-ms", type=int, help="required for first sync; otherwise resumes with one-minute overlap")
    ledger_sync.add_argument("--end-ms", type=int)
    ledger_report = commands.add_parser("ledger-report", help="report observed realized cash flows with coverage limits")
    ledger_report.add_argument("--directory", required=True)
    ledger_report.add_argument("--start-ms", type=int, required=True)
    ledger_report.add_argument("--end-ms", type=int, required=True)
    monitor = commands.add_parser("watch", help="collect unsigned snapshots and existing Rust notification events")
    for option in ("network", "account", "analytics-bin", "directory"):
        monitor.add_argument("--" + option, required=True)
    monitor.add_argument("--interval-seconds", type=int, default=30)
    monitor.add_argument("--duration-seconds", type=int, default=3600)
    monitor.add_argument("--websocket", action="store_true")
    monitor.add_argument("--once", action="store_true")
    monitor.add_argument("--ledger-dir")
    monitor.add_argument("--ledger-start-ms", type=int)
    recovery = commands.add_parser("plan-recovery", help="derive an unsigned Rust recovery handoff from an owned bracket")
    for option in ("entry-plan", "state", "limits", "recovery-bin", "signer-socket", "output"):
        recovery.add_argument("--" + option, required=True)
    equity = commands.add_parser("equity-checkpoint", help="archive a fresh unsigned Unified-USDC valuation")
    for option in ("network", "account", "output"):
        equity.add_argument("--" + option, required=True)
    equity_report = commands.add_parser("equity-reconcile", help="explain a checkpoint interval and preserve residuals")
    for option in ("directory", "before", "after", "output"):
        equity_report.add_argument("--" + option, required=True)
    equity_report.add_argument("--tolerance-usdc", default="0.01")
    args = parser.parse_args()
    if args.command == "signer":
        serve(args.policy, args.keystore, args.state, args.socket)
    elif args.command == "status":
        if args.cache_dir:
            result = cached(args.cache_dir, args.network, args.account.lower(), args.max_age_ms)
            print(json.dumps(result, indent=2))
            require(result["usable"], "cached observation is unavailable or stale")
        else:
            print(json.dumps(snapshot(Venue(args.network), args.account.lower()), indent=2))
    elif args.command == "adopt":
        record = adoption(Venue(args.network), args.account.lower(), args.coin)
        print(json.dumps(record, indent=2))
        approval = digest(record)
        print("Adoption SHA-256:", approval)
        require(input("Type that SHA-256 to adopt this observed position and listed exit IDs: ").strip() == approval,
                "adoption not approved")
        write(args.output, record)
    elif args.command == "plan-close":
        record = read(args.adoption)
        plan = plan_close(Venue(record["network"]), record, args.agent.lower(), args.key_id,
                          args.quantity, args.slippage_bps)
        print(json.dumps(save_plan(args.output, plan), indent=2))
    elif args.command == "plan-open":
        value = read(args.input)
        plan = plan_open(Venue(value["network"]), value, args.agent.lower(), args.key_id,
                         args.analytics_bin, args.journal_bin)
        print(json.dumps(save_plan(args.output, plan), indent=2))
    elif args.command == "plan-protect":
        record = read(args.adoption)
        plan = plan_protect(Venue(record["network"]), record, args.agent.lower(), args.key_id,
                            args.take_profit, args.stop_loss, args.exit_slippage_percent)
        print(json.dumps(save_plan(args.output, plan), indent=2))
    elif args.command == "execute":
        plan = read(args.plan)
        execute = {"open": execute_open, "close": execute_close, "protect": execute_protect}[plan["operation"]]
        result = execute(Venue(plan["policy"]["network"]), plan, args.socket, args.state)
        print(json.dumps(result, indent=2))
        require(result["status"] in ("open", "closed", "residual_position", "protection_verified"), "execution needs reconciliation")
    elif args.command == "reconcile":
        plan = read(args.plan)
        venue = Venue(plan["policy"]["network"])
        if plan["operation"] in ("open", "protect"):
            require(args.state, "entry reconciliation requires its state directory")
            reconcile_operation = reconcile_open if plan["operation"] == "open" else reconcile_protect
            result = reconcile_operation(venue, plan, args.state)
        else:
            result = reconcile_pending(venue, plan, args.state) if args.state else reconcile(venue, plan)
        print(json.dumps(result, indent=2))
    elif args.command == "ledger-sync":
        start = args.start_ms
        if start is None:
            previous = read(Path(args.directory) / "ledger.json")
            start = max(0, max(w["end_ms"] for w in previous["windows"]) - 60000)
        ledger = sync(Venue(args.network), args.account.lower(), args.directory, start,
                      now_ms() if args.end_ms is None else args.end_ms)
        print(json.dumps({"window": ledger["windows"][-1], "records": {k: len(v) for k, v in ledger["records"].items()}}, indent=2))
    elif args.command == "ledger-report":
        ledger = read(Path(args.directory) / "ledger.json")
        print(json.dumps(report(Venue(ledger["network"]), ledger, args.start_ms, args.end_ms), indent=2))
    elif args.command == "equity-checkpoint":
        from .equity import checkpoint
        point = checkpoint(Venue(args.network), args.account.lower())
        write(args.output, point)
        print(json.dumps({k: v for k, v in point.items() if k != "observation"}, indent=2))
    elif args.command == "equity-reconcile":
        from .equity import reconcile_equity
        ledger = read(Path(args.directory) / "ledger.json")
        result = reconcile_equity(Venue(ledger["network"]), ledger, read(args.before), read(args.after), args.tolerance_usdc)
        write(args.output, result)
        print(json.dumps(result, indent=2))
    elif args.command == "plan-recovery":
        from .recovery import prepare
        entry_plan = read(args.entry_plan)
        plan = prepare(Venue(entry_plan["policy"]["network"]), entry_plan, args.state, read(args.limits),
                       args.recovery_bin, args.signer_socket)
        root = Path(args.output)
        from .store import private_dir
        private_dir(root)
        write(root / "handoff.json", plan)
        write(root / "policy.json", plan["policy"])
        write(root / "recovery-config.json", plan["config"])
        print(json.dumps({"directory": str(root), "policy_sha256": digest(plan["policy"]),
                          "handoff_sha256": digest(plan), "mainnet_authorized": False}, indent=2))
    elif args.command == "watch":
        result = watch(args.analytics_bin, args.network, args.account.lower(), args.directory,
                       args.interval_seconds, args.duration_seconds, args.websocket, args.once,
                       args.ledger_dir, args.ledger_start_ms)
        require(result["snapshot"] is not None, "collector ended with unknown state")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
    except Exception as error:
        # Custody libraries can include sensitive inputs in exception messages.
        print("local operator failed (" + type(error).__name__ + "); inspect unsigned state and configuration", file=sys.stderr)
        sys.exit(1)
