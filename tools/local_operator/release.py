"""Attended bracket release binding. Supplied human claims are not auto-attestations."""
import importlib.metadata
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
import platform
import re
import subprocess

from .store import digest, read, require

SCENARIOS = {"full_lifecycle", "partial_lifecycle", "fault_restart", "security_review", "alert_and_manual_recovery", "mainnet_accounting"}
PROGRAMS = {"analytics": "hypercarry", "journal": "hypercarry-bracket-journal", "recovery": "hypercarry-testnet-recovery"}


def candidate(repo, binaries):
    from .entry import artifact
    root = Path(repo).resolve(strict=True)
    def git(*args):
        return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()
    require(not git("status", "--porcelain"), "candidate requires a clean committed source tree")
    require(set(binaries) == set(PROGRAMS), "candidate needs analytics, journal and recovery binaries")
    sdk = importlib.metadata.version("hyperliquid-python-sdk")
    require(sdk == "0.24.0", "unreviewed SDK")
    return {"schema_version": 1, "kind": "local_bracket_candidate", "source_commit": git("rev-parse", "HEAD"),
            "cargo_lock": artifact(root / "Cargo.lock"), "binaries": {k: artifact(v) for k, v in binaries.items()},
            "sdk_version": sdk, "python_version": platform.python_version(),
            "python_packages": sorted([d.metadata["Name"].lower(), d.version]
                                      for d in importlib.metadata.distributions())}


def verify_candidate(value, repo):
    require(value == candidate(repo, {k: v["path"] for k, v in value["binaries"].items()}), "candidate source/artifacts changed")


def evidence(record, root, expected, now):
    from .entry import artifact
    required = {"schema_version", "candidate_sha256", "scenario", "outcome", "operator", "reviewer",
                "performed_at_ms", "observations", "artifacts", "session_id"}
    require(set(record) == required and record["schema_version"] == 1
            and record["candidate_sha256"] == expected and record["scenario"] in SCENARIOS
            and record["outcome"] == "passed", "evidence schema, candidate or outcome mismatch")
    for key in ("operator", "reviewer", "observations"):
        require(isinstance(record[key], str) and 0 < len(record[key].strip()) <= 16000, "missing evidence " + key)
    require(record["operator"].strip().casefold() != record["reviewer"].strip().casefold(), "separate human reviewer required")
    require(type(record["performed_at_ms"]) is int and 0 < record["performed_at_ms"] <= now, "invalid evidence time")
    session = record["scenario"] in {"full_lifecycle", "partial_lifecycle", "fault_restart"}
    require((isinstance(record["session_id"], str) and 0 < len(record["session_id"]) <= 200) if session
            else record["session_id"] is None, "invalid session identity")
    require(isinstance(record["artifacts"], list) and 1 <= len(record["artifacts"]) <= 32, "retained evidence artifacts required")
    seen = set()
    for item in record["artifacts"]:
        require(set(item) == {"path", "sha256"}, "invalid evidence artifact")
        name = item["path"]
        require(isinstance(name, str) and name and "\\" not in name, "invalid artifact path")
        relative = PurePosixPath(name)
        require(not relative.is_absolute() and ".." not in relative.parts and str(relative) == name
                and name not in seen, "unsafe or duplicate evidence path")
        seen.add(name)
        path = root / name
        require(not any(p.is_symlink() for p in (path, *path.parents)), "evidence symlink refused")
        require(artifact(path)["sha256"] == item["sha256"], "evidence artifact changed")
    return record


class Release:
    """Exact operator-pinned release, rechecked at signing and submission boundaries."""
    def __init__(self, path, expected, plan, repo, *, enabled=False):
        require(enabled, "mainnet requires explicit --enable-mainnet")
        require(isinstance(expected, str) and re.fullmatch(r"[0-9a-f]{64}", expected), "trusted release digest required")
        self.path, self.expected, self.plan, self.repo = Path(path).absolute(), expected, plan, Path(repo).resolve(strict=True)
        self.value = read(self.path)
        self.check()

    def check(self):
        from .signer import now_ms, number, address, validate_policy
        from .workflow import verify_plan
        value = read(self.path)
        require(digest(value) == self.expected and value == self.value, "release changed after approval")
        fields = {"schema_version", "kind", "candidate", "plan_digest", "account", "agent", "testnet_agent",
                  "operations_dir", "signer_dir", "stop_file", "expires_at_ms", "decision", "operator", "reviewer", "evidence", "checks"}
        require(set(value) == fields and value["schema_version"] == 1 and value["kind"] == "attended_mainnet_bracket_release"
                and value["decision"] == "approve", "not an approved bracket release")
        verify_candidate(value["candidate"], self.repo)
        verify_plan(self.plan)
        require(self.plan.get("candidate_sha256") == digest(value["candidate"]), "plan candidate mismatch")
        p = self.plan["policy"]
        require(value["plan_digest"] == digest(self.plan) and p["network"] == "mainnet"
                and value["account"] == p["account"] and value["agent"] == p["signer_address"], "release plan/account/agent mismatch")
        require(address(value["testnet_agent"]) not in (address(value["account"]), address(value["agent"])), "dedicated mainnet agent required")
        require(all(isinstance(value[k], str) and value[k].strip() for k in ("operator", "reviewer"))
                and value["operator"].strip().casefold() != value["reviewer"].strip().casefold(), "release reviewer must differ from operator")
        now = now_ms()
        from tools.check_dependency_policy import EXPIRES
        require(datetime.fromtimestamp(now / 1000, timezone.utc).date() <= EXPIRES,
                "candidate's dependency maintenance exception expired")
        validate_policy(p, now, mainnet_preview=True)
        require(type(value["expires_at_ms"]) is int and now < value["expires_at_ms"] <= now + 3_600_000
                and p["expires_at_ms"] <= value["expires_at_ms"], "release expired or outside one-hour limit")
        for key in ("operations_dir", "signer_dir", "stop_file"):
            path = Path(value[key])
            require(path.is_absolute() and str(path.resolve()) == value[key] and not any(x.is_symlink() for x in (path,*path.parents)), "unsafe release path")
        require(value["operations_dir"] != value["signer_dir"], "separate operator and signer state required")
        require(not Path(value["stop_file"]).exists(), "mainnet STOP is set")
        require(value["checks"] == {"offline_tests": True, "dependency_policy": True, "advisory_audit": True}, "passing candidate checks required")
        records = value["evidence"]
        require(isinstance(records, list) and len(records) == len(SCENARIOS), "all release scenarios required")
        scopes, sessions = set(), set()
        for record in records:
            evidence(record, self.path.parent, digest(value["candidate"]), now)
            require(record["scenario"] not in scopes, "duplicate evidence scenario")
            scopes.add(record["scenario"])
            if record["session_id"] is not None:
                require(record["session_id"] not in sessions, "three distinct credentialed sessions required")
                sessions.add(record["session_id"])
        require(scopes == SCENARIOS and len(sessions) == 3, "incomplete release evidence")
        require(self.plan["operation"] in ("open", "close"), "pilot permits entry/full close only")
        require(":" not in p["coin"] and not p["coin"].startswith("@") and 0 <= p["asset"] < 100_000
                and p["leverage"] == 1, "pilot is native isolated 1x")
        if self.plan["operation"] == "open":
            require(number(self.plan["calculated"]["input"]["notional_budget"]) <= 20
                    and number(p["quantity"]) * number(p["entry_limit"]) <= 20, "pilot entry exceeds 20 USDC planned bound")
            require(self.plan["tools"] == {k: value["candidate"]["binaries"][k] for k in ("analytics", "journal")}, "plan binaries differ from candidate")
        else:
            require(self.plan["adoption"].get("origin") == "owned_bracket_entry"
                    and self.plan["adoption"].get("entry_plan_digest"), "close requires owned pilot entry")
            session = self.plan.get("pilot_entry_session")
            require(isinstance(session, str) and re.fullmatch(r"[a-zA-Z0-9_-]{1,64}", session), "owned entry session required")
            original = read(Path(value["operations_dir"]) / session / "plan.json")
            adoption = read(Path(value["operations_dir"]) / session / "position.json")
            require(original["operation"] == "open" and original["policy"]["network"] == "mainnet"
                    and original["policy"]["account"] == p["account"] and original["policy"]["signer_address"] == p["signer_address"]
                    and original["policy"]["coin"] == p["coin"] and digest(original) == adoption["entry_plan_digest"]
                    and adoption == self.plan["adoption"], "close ownership differs from durable pilot entry")
            from .venue import find_position
            position = find_position(self.plan["baseline"], p["coin"])
            require(position and number(p["quantity"]) == abs(number(position["szi"])), "pilot close must cover the full residual")
        return value

    def authorize(self, policy, *, plan=None, operations=None, signer=None):
        self.check()
        require(policy == self.plan["policy"], "policy differs from released exact plan")
        if plan is not None: require(plan == self.plan, "plan differs from release")
        if operations is not None: require(str(Path(operations).absolute()) == self.value["operations_dir"], "wrong operator state directory")
        if signer is not None: require(str(Path(signer).absolute()) == self.value["signer_dir"], "wrong signer state directory")

    def authorize_action(self, action):
        self.check()
        p = self.plan["policy"]
        if self.plan["operation"] == "open":
            if action.get("type") == "updateLeverage":
                require(action == {"type": "updateLeverage", "asset": p["asset"], "isCross": False, "leverage": 1}, "unapproved configuration")
            elif action.get("type") == "order" and any(not o.get("r") for o in action.get("orders", [])):
                require(action == self.plan["action"], "entry differs from released exact action")
            else:
                require(action.get("type") == "order" and action.get("orders")
                        and all(o.get("r") is True and "trigger" in o.get("t", {}) for o in action["orders"]),
                        "entry release permits only bounded protection repair after entry")
        else:
            require(action == self.plan["action"] or action.get("type") == "cancel", "close release permits exact close and owned cleanup only")
        # Validate every field, even for calls made directly to the venue adapter.
        from .signer import validate_action
        state = {"configured": False, "entry_signed": False, "canceled": [], "cloids": [], "close_quantity": "0"}
        validate_action(action, p, state)


def execution_scope(venue, policy=None, plan=None, operations=None):
    """Mainnet only through the distinct explicitly released venue; testnet unchanged."""
    require(venue.network in ("testnet", "mainnet"), "unknown execution network")
    if policy is not None: require(venue.network == policy["network"], "execution network mismatch")
    if venue.network == "mainnet":
        from .mainnet import MainnetVenue
        require(isinstance(venue, MainnetVenue), "mainnet execution is not released")
        venue.release.authorize(policy or venue.release.plan["policy"], plan=plan, operations=operations)
