"""Separate, default-off attended native bracket pilot. No release is generated here."""
import argparse
import json
import subprocess
from pathlib import Path

from .release import Release, candidate, verify_candidate
from .signer import now_ms, number, serve, validate_policy, wire_action
from .store import digest, private_dir, read, require, write
from .venue import Venue, fresh_action_evidence, verify_agent
from .workflow import save_plan, seal_plan
from .entry import final_entry_evidence


class MainnetVenue(Venue):
    def __init__(self, release):
        require(isinstance(release, Release), "mainnet venue needs a bound release")
        release.check()
        super().__init__("mainnet")
        self.release = release

    def exchange(self, signed):
        from hyperliquid.utils.signing import recover_agent_or_user_from_l1_action
        release, p = self.release, self.release.plan["policy"]
        require(self.network == "mainnet" and self.base == "https://api.hyperliquid.xyz/", "mainnet endpoint changed")
        release.authorize(p)
        require(set(signed) == {"action", "nonce", "expiresAfter", "vaultAddress", "signature"}
                and signed["vaultAddress"] is None, "unsupported signed envelope")
        release.authorize_action(signed["action"])
        require(type(signed["nonce"]) is int and type(signed["expiresAfter"]) is int
                and now_ms() - 30_000 <= signed["nonce"] <= now_ms()
                and 0 < signed["expiresAfter"] - signed["nonce"] <= p.get("action_ttl_ms", 10_000)
                and now_ms() < signed["expiresAfter"] <= p["expires_at_ms"], "invalid signed timing")
        require(recover_agent_or_user_from_l1_action(wire_action(signed["action"]), signed["signature"], None,
                signed["nonce"], signed["expiresAfter"], True).lower() == p["signer_address"], "wrong mainnet signing domain/agent")
        fresh_action_evidence(self, p, signed["action"])
        if signed["action"]["type"] == "order" and any(not o["r"] for o in signed["action"]["orders"]):
            final_entry_evidence(self, release.plan)
        verify_agent(self, p, signed["expiresAfter"])
        # Last STOP/artifact check after signing and every final network observation.
        release.check()
        require(now_ms() + 5000 <= signed["expiresAfter"], "insufficient venue receipt lifetime")
        root = private_dir(Path(release.value["operations_dir"]) / "mainnet-wire-reservations")
        write(root / (p["signer_address"] + "-" + str(signed["nonce"]) + ".json"),
              {"release_digest": release.expected, "action_digest": digest(signed["action"]), "nonce": signed["nonce"]})
        if signed["action"]["type"] == "order" and any(not o["r"] for o in signed["action"]["orders"]):
            write(root / ("entry-" + p["session_id"] + ".json"), {"release_digest": release.expected, "nonce": signed["nonce"]})
        # One attempt. A lost response retains both the account and wire reservation.
        return self._post("exchange", signed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    freeze = sub.add_parser("freeze-candidate", help="record clean source and binary identities; no approval")
    freeze.add_argument("--repo", default=str(Path(__file__).resolve().parents[2]))
    freeze.add_argument("--binary-dir", required=True)
    freeze.add_argument("--output", required=True)
    for kind in ("preview-open", "preview-close"):
        preview = sub.add_parser(kind, help="unsigned mainnet plan; cannot sign or submit")
        preview.add_argument("--candidate", required=True)
        preview.add_argument("--candidate-sha256", required=True)
        preview.add_argument("--input" if kind == "preview-open" else "--adoption", required=True)
        preview.add_argument("--agent", required=True)
        preview.add_argument("--key-id", required=True)
        preview.add_argument("--output", required=True)
    for kind in ("check-release", "signer", "execute"):
        command = sub.add_parser(kind)
        command.add_argument("--enable-mainnet", action="store_true", required=True)
        command.add_argument("--release", required=True)
        command.add_argument("--release-sha256", required=True)
        command.add_argument("--plan", required=True)
        if kind == "signer": command.add_argument("--keystore", required=True)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    if args.command == "freeze-candidate":
        from .release import PROGRAMS
        binary_dir = Path(args.binary_dir).resolve()
        require(binary_dir.name == "release", "freeze builds a release directory")
        require(not subprocess.check_output(["git", "-C", args.repo, "status", "--porcelain"], text=True).strip(),
                "commit changes before freezing")
        subprocess.run(["cargo", "build", "--workspace", "--all-features", "--locked", "--release",
                        "--target-dir", str(binary_dir.parent)], cwd=args.repo, check=True)
        value = candidate(args.repo, {k: Path(args.binary_dir) / v for k, v in PROGRAMS.items()})
        write(args.output, value)
        print(json.dumps({"candidate": args.output, "sha256": digest(value), "mainnet_authorized": False}, indent=2))
        return
    if args.command.startswith("preview-"):
        from .entry import plan_open
        from .workflow import plan_close
        value = read(args.candidate)
        require(digest(value) == args.candidate_sha256, "candidate digest mismatch")
        verify_candidate(value, repo)
        if args.command == "preview-open":
            inputs = read(args.input)
            require(inputs["network"] == "mainnet" and ":" not in inputs["coin"]
                    and 0 < number(inputs["notional_budget"]) <= 20, "native pilot budget must be at most 20 USDC")
            plan = plan_open(Venue("mainnet"), inputs, args.agent.lower(), args.key_id,
                            value["binaries"]["analytics"]["path"], value["binaries"]["journal"]["path"], mainnet_preview=True)
        else:
            record = read(args.adoption)
            entry = read(Path(args.adoption).parent / "plan.json")
            require(entry["operation"] == "open" and entry["policy"]["network"] == record["network"] == "mainnet"
                    and record["origin"] == "owned_bracket_entry" and record["entry_plan_digest"] == digest(entry),
                    "mainnet close needs the preserved owned entry")
            plan = plan_close(Venue("mainnet"), record, args.agent.lower(), args.key_id, mainnet_preview=True)
            plan["pilot_entry_session"] = entry["policy"]["session_id"]
        plan["candidate_sha256"] = digest(value)
        plan["policy"]["action_ttl_ms"] = 30_000
        validate_policy(plan["policy"], now_ms(), mainnet_preview=True)
        seal_plan(plan)
        print(json.dumps({**save_plan(args.output, plan), "mainnet_authorized": False}, indent=2))
        return
    plan = read(args.plan)
    release = Release(args.release, args.release_sha256, plan, repo, enabled=args.enable_mainnet)
    if args.command == "check-release":
        print(json.dumps({"release_digest": release.expected, "eligible_for_local_approval": True,
                          "orders_submitted": False, "limitations": "Verifies supplied evidence bindings, not human claims or live readiness."}))
    elif args.command == "signer":
        directory = Path(release.value["signer_dir"])
        policy = Path(args.plan).with_suffix(".policy.json")
        serve(policy, args.keystore, directory, directory / "agent.sock", release=release)
    elif args.command == "execute":
        from .entry import execute_open
        from .workflow import execute_close
        venue = MainnetVenue(release)
        run = execute_open if plan["operation"] == "open" else execute_close
        result = run(venue, plan, Path(release.value["signer_dir"]) / "agent.sock", release.value["operations_dir"])
        print(json.dumps(result, indent=2))
        require(result["status"] in ("open", "closed"), "mainnet pilot requires reconciliation; never retry")


if __name__ == "__main__":
    # Use the canonical module's class identity: execution_scope imports it too.
    # Running this file as __main__ must not create a second MainnetVenue type.
    from .mainnet import main as run_main
    try: run_main()
    except KeyboardInterrupt: raise SystemExit(130)
    except Exception as error:
        # Never echo request or custody exceptions, which can contain signatures.
        print("mainnet pilot refused (" + type(error).__name__ + "); preserve state and reconcile")
        raise SystemExit(1)
