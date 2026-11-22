"""Bounded agent signer compatible with the Rust Unix socket protocol.

The owner unlocks an encrypted agent keystore once in this separate process.
Mainnet requires the separate release gate. No raw-key argument, owner-wallet
signing, or arbitrary digest API.
"""
import getpass
import json
import os
from pathlib import Path
import re
import socket
import signal
import time
from decimal import Decimal

from .store import digest, lock, private_dir, read, require, write


def now_ms():
    return time.time_ns() // 1_000_000


def number(value):
    require(isinstance(value, str), "financial values must be decimal strings")
    result = Decimal(value)
    require(result.is_finite(), "nonfinite financial value")
    return result


def address(value):
    require(isinstance(value, str) and re.fullmatch(r"0x[0-9a-f]{40}", value), "invalid lowercase address")
    return value


def exact_keys(value, keys):
    require(isinstance(value, dict) and set(value) == set(keys.split()), "unsupported fields")


def wire_action(action):
    """Restore SDK MessagePack field order after sorted JSON persistence."""
    kind = action["type"]
    if kind == "updateLeverage":
        return {k: action[k] for k in ("type", "asset", "isCross", "leverage")}
    if kind == "cancel":
        return {"type": kind, "cancels": [{k: c[k] for k in ("a", "o")} for c in action["cancels"]]}
    if kind == "cancelByCloid":
        return {"type": kind, "cancels": [{k: c[k] for k in ("asset", "cloid")} for c in action["cancels"]]}
    require(kind == "order", "unsupported signing action")
    orders = []
    for item in action["orders"]:
        order = {k: item[k] for k in ("a", "b", "p", "s", "r", "t", "c")}
        if "trigger" in order["t"]:
            order["t"] = {"trigger": {k: item["t"]["trigger"][k]
                                       for k in ("isMarket", "triggerPx", "tpsl")}}
        orders.append(order)
    return {"type": kind, "orders": orders, "grouping": action["grouping"]}


def validate_policy(p, now, *, mainnet_preview=False):
    fields = "schema_version network account signer_address key_id session_id expires_at_ms asset coin " \
               "is_buy quantity entry_limit tp sl tp_limit sl_limit close_min close_max leverage " \
               "allow_entry cancel_oids max_actions plan_digest reserve_usdc"
    exact_keys(p, fields + (" recovery_cloids" if "recovery_cloids" in p else "")
               + (" action_ttl_ms" if "action_ttl_ms" in p else ""))
    require(type(p.get("action_ttl_ms", 10_000)) is int
            and 10_000 <= p.get("action_ttl_ms", 10_000) <= 30_000,
            "action lifetime must be between ten and thirty seconds")
    if "recovery_cloids" in p:
        require(p["allow_entry"] is False and p["cancel_oids"] == []
                and isinstance(p["recovery_cloids"], list) and len(p["recovery_cloids"]) == 3
                and len(set(p["recovery_cloids"])) == 3
                and all(isinstance(c, str) and re.fullmatch(r"0x[0-9a-f]{32}", c) for c in p["recovery_cloids"]),
                "recovery needs exactly three owned client IDs and no entry/numeric cancel authority")
    require(p["schema_version"] == 1 and (p["network"] == "testnet" or mainnet_preview and p["network"] == "mainnet"), "mainnet policy needs the separate release path")
    require(isinstance(p["plan_digest"], str) and re.fullmatch(r"[0-9a-f]{64}", p["plan_digest"])
            and number(p["reserve_usdc"]) >= 0, "invalid full-plan binding/reserve")
    require(address(p["account"]) != address(p["signer_address"]), "use a dedicated agent, never the owner key")
    require(re.fullmatch(r"[a-zA-Z0-9_-]{1,64}", p["session_id"] or ""), "invalid session id")
    require(isinstance(p["key_id"], str) and 0 < len(p["key_id"]) <= 128, "invalid key alias")
    require(type(p["expires_at_ms"]) is int and now < p["expires_at_ms"] <= now + 3_600_000,
            "session must expire within one hour")
    require(type(p["asset"]) is int and p["asset"] >= 0 and isinstance(p["coin"], str), "invalid asset")
    require(type(p["is_buy"]) is bool and type(p["allow_entry"]) is bool, "invalid side/entry policy")
    require(p["leverage"] == 1 and type(p["leverage"]) is int, "initial profile is isolated 1x")
    require(type(p["max_actions"]) is int and 1 <= p["max_actions"] <= 16, "invalid action budget")
    require(isinstance(p["cancel_oids"], list) and len(p["cancel_oids"]) <= 8
            and all(type(o) is int and o > 0 for o in p["cancel_oids"]), "invalid owned cancel ids")
    for key in ("quantity", "entry_limit", "tp", "sl", "tp_limit", "sl_limit", "close_min", "close_max"):
        require(number(p[key]) > 0, "nonpositive policy value")
    require(number(p["close_min"]) <= number(p["close_max"]), "inverted close collar")
    require((number(p["tp"]) > number(p["sl"])) == p["is_buy"], "inverted exit triggers")
    for key in ("tp", "sl"):
        require((number(p[key + "_limit"]) <= number(p[key])) if p["is_buy"]
                else (number(p[key + "_limit"]) >= number(p[key])), "unmarketable exit bound")


def validate_action(action, p, state):
    kind = action.get("type")
    if kind == "cancelByCloid":
        exact_keys(action, "type cancels")
        require(isinstance(action["cancels"], list) and len(action["cancels"]) == 1, "one recovery cancel per request")
        item = action["cancels"][0]
        exact_keys(item, "asset cloid")
        require(item["asset"] == p["asset"] and item["cloid"] in p.get("recovery_cloids", [])
                and item["cloid"] not in state["canceled"], "unapproved or repeated recovery cancellation")
        state["canceled"].append(item["cloid"])
        return
    if kind == "updateLeverage":
        exact_keys(action, "type asset isCross leverage")
        require(p["allow_entry"] and not state["configured"] and not state["entry_signed"]
                and action["asset"] == p["asset"] and action["isCross"] is False
                and action["leverage"] == p["leverage"], "configuration outside approval")
        state["configured"] = True
        return
    if kind == "cancel":
        exact_keys(action, "type cancels")
        require(0 < len(action["cancels"]) <= 3, "invalid cancel batch")
        for item in action["cancels"]:
            exact_keys(item, "a o")
            require(item["a"] == p["asset"] and item["o"] in p["cancel_oids"]
                    and item["o"] not in state["canceled"], "unapproved or repeated cancellation")
            state["canceled"].append(item["o"])
        return
    require(kind == "order", "unsupported action")
    exact_keys(action, "type orders grouping")
    orders = action["orders"]
    require(isinstance(orders, list) and 1 <= len(orders) <= 3, "invalid order batch")
    entries = []
    triggers = []
    for order in orders:
        exact_keys(order, "a b p s r t c")
        require(order["a"] == p["asset"] and type(order["b"]) is bool and type(order["r"]) is bool,
                "wrong asset or side type")
        require(re.fullmatch(r"0x[0-9a-f]{32}", order["c"] or "")
                and order["c"] not in state["cloids"], "invalid/reused client order id")
        state["cloids"].append(order["c"])
        qty, price = number(order["s"]), number(order["p"])
        require(0 < qty <= number(p["quantity"]) and price > 0, "size or price outside approval")
        if not order["r"]:
            entries.append(order)
            require(p["allow_entry"] and not state["entry_signed"] and order["b"] == p["is_buy"]
                    and qty == number(p["quantity"]) and price == number(p["entry_limit"])
                    and order["t"] == {"limit": {"tif": "Ioc"}}, "entry outside exact approval")
        else:
            require(order["b"] != p["is_buy"], "reduce-only side must close approved exposure")
            if "trigger" in order["t"]:
                require("recovery_cloids" not in p, "recovery policy cannot create or amend protection")
                exact_keys(order["t"], "trigger")
                trigger = order["t"]["trigger"]
                exact_keys(trigger, "isMarket triggerPx tpsl")
                key = trigger["tpsl"]
                require(key in ("tp", "sl") and trigger["isMarket"] is True
                        and number(trigger["triggerPx"]) == number(p[key])
                        and price == number(p[key + "_limit"]), "exit outside approval")
                triggers.append(key)
            else:
                require(order["t"] == {"limit": {"tif": "Ioc"}}
                        and number(p["close_min"]) <= price <= number(p["close_max"]),
                        "close outside approved collar")
                state["close_quantity"] = str(number(state["close_quantity"]) + qty)
                require(number(state["close_quantity"]) <= number(p["quantity"]), "close budget exhausted")
    if entries:
        require(len(entries) == 1 and len(orders) == 3 and orders[0] == entries[0]
                and triggers == ["tp", "sl"] and action["grouping"] == "normalTpsl"
                and all(number(o["s"]) == number(p["quantity"]) for o in orders), "entry requires full bracket")
        state["entry_signed"] = True
    else:
        require(action["grouping"] == "na" and len(triggers) == len(set(triggers)), "invalid exit grouping")


class Session:
    def __init__(self, policy, directory, now=None, *, release=None):
        self.release = release
        if policy["network"] == "mainnet":
            from .release import Release
            require(isinstance(release, Release), "mainnet signer requires bound release")
            release.authorize(policy, signer=directory)
        validate_policy(policy, now_ms() if now is None else now, mainnet_preview=release is not None)
        self.policy = policy
        self.directory = private_dir(directory)
        self.path = self.directory / (policy["session_id"] + ".json")
        self.nonce_path = self.directory / (policy["signer_address"] + "-nonce.json")
        self.policy_digest = digest(policy)

    def reserve(self, request, now):
        p = self.policy
        if self.release is not None:
            self.release.authorize(p, signer=self.directory)
            self.release.authorize_action(request["action"])
        exact_keys(request, "schema_version network key_id signer_address nonce expires_after action")
        require(request["schema_version"] == 1 and request["network"] == p["network"]
                and request["key_id"] == p["key_id"] and request["signer_address"] == p["signer_address"],
                "signing identity mismatch")
        require(type(request["nonce"]) is int and now - 5000 <= request["nonce"] <= now
                and type(request["expires_after"]) is int
                and now < request["expires_after"] <= min(now + p.get("action_ttl_ms", 10_000), p["expires_at_ms"]),
                "stale nonce or expiry")
        state = read(self.path) if self.path.exists() else {
            "policy_digest": self.policy_digest, "actions": 0, "entry_signed": False,
            "configured": False, "canceled": [], "cloids": [], "close_quantity": "0"}
        require(state["policy_digest"] == self.policy_digest, "session policy changed")
        require(state["actions"] < p["max_actions"], "session action budget exhausted")
        last = read(self.nonce_path)["nonce"] if self.nonce_path.exists() else 0
        require(request["nonce"] > last, "nonce replay or regression")
        validate_action(request["action"], p, state)
        state["actions"] += 1
        # Consume nonce and action before custody access; uncertain signing cannot be replayed.
        write(self.nonce_path, {"nonce": request["nonce"]}, replace=True)
        write(self.path, state, replace=True)


def receive(stream):
    stream.settimeout(5)
    chunks = bytearray()
    while True:
        chunk = stream.recv(4096)
        if not chunk:
            break
        chunks.extend(chunk)
        require(len(chunks) <= 65536, "oversized signer request")
    return json.loads(chunks)


def serve(policy_path, keystore_path, directory, socket_path, *, release=None):
    import importlib.metadata
    require(importlib.metadata.version("hyperliquid-python-sdk") == "0.24.0", "unreviewed SDK version")
    from eth_account import Account
    from hyperliquid.utils.signing import sign_l1_action
    policy = read(policy_path)
    def stop_session(_signal, _frame):
        raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM, stop_session)
    session = Session(policy, directory, release=release)
    socket_path = Path(socket_path).absolute()
    require(socket_path.parent == session.directory and not socket_path.exists(), "socket must be new in signer state directory")
    print(json.dumps({"approval": policy, "sha256": session.policy_digest}, indent=2), flush=True)
    require(input("Type the policy SHA-256 to approve this " + policy["network"] + " session: ").strip() == session.policy_digest,
            "session was not approved")
    with lock(session.directory / (policy["signer_address"] + ".lock")):
        password = getpass.getpass("Agent keystore password (local only): ")
        try:
            key = bytearray(Account.decrypt(read(keystore_path), password))
        finally:
            password = None
        listener = socket.socket(socket.AF_UNIX)
        try:
            wallet = Account.from_key(key)
            require(wallet.address.lower() == policy["signer_address"], "wrong agent keystore")
            listener.bind(str(socket_path))
            os.chmod(socket_path, 0o600)
            listener.listen(1)
            listener.settimeout(1)
            print(policy["network"] + " agent signer ready; session expiry is enforced.", flush=True)
            while now_ms() < policy["expires_at_ms"]:
                try:
                    connection, _ = listener.accept()
                except TimeoutError:
                    continue
                with connection:
                    try:
                        request = receive(connection)
                        session.reserve(request, now_ms())
                        action = wire_action(request["action"])
                        signature = sign_l1_action(wallet, action, None,
                                                   request["nonce"], request["expires_after"], policy["network"] == "mainnet")
                        require(now_ms() < request["expires_after"], "signature expired")
                        response = {"schema_version": 1, "network": policy["network"], "key_id": policy["key_id"],
                                    "signer_address": policy["signer_address"], "signed_request": {
                                        "action": action, "nonce": request["nonce"],
                                        "expiresAfter": request["expires_after"], "vaultAddress": None,
                                        "signature": signature}}
                        connection.sendall(json.dumps(response).encode())
                    except (ValueError, KeyError, TypeError, OSError):
                        # Never echo a request, signature, password or backend exception.
                        connection.sendall(b'{"error":"signing refused; inspect unsigned state"}')
        finally:
            listener.close()
            if socket_path.exists():
                socket_path.unlink()
            for index in range(len(key)):
                key[index] = 0
            # Python/SDK copies cannot be guaranteed erased; process lifetime bounds exposure.
