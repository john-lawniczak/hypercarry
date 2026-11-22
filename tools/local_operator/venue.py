"""Bounded Hyperliquid reads and an explicit testnet-only write boundary."""
from concurrent.futures import ThreadPoolExecutor
from decimal import Decimal, ROUND_DOWN, ROUND_UP
import json
import os
import socket
import stat
import time
import urllib.request

from .signer import address, now_ms, number, wire_action
from .store import read, require, write
from .release import execution_scope


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("API redirects are not accepted")


class Venue:
    def __init__(self, network):
        require(network in ("mainnet", "testnet"), "unknown network")
        self.network = network
        self.base = "https://api.hyperliquid" + ("-testnet" if network == "testnet" else "") + ".xyz/"

    def post(self, endpoint, payload):
        require(endpoint == "info" or endpoint == "exchange" and self.network == "testnet", "mainnet writes require the released adapter")
        return self._post(endpoint, payload)

    def _post(self, endpoint, payload):
        # One attempt only, including for exchange writes. Never log signed payloads.
        request = urllib.request.Request(self.base + endpoint, json.dumps(payload).encode(),
                                         {"Content-Type": "application/json"})
        with urllib.request.build_opener(NoRedirect).open(request, timeout=10) as response:
            body = response.read(16 * 1024 * 1024 + 1)
        require(len(body) <= 16 * 1024 * 1024, "oversized API response")
        return json.loads(body)

    def info(self, kind, **fields):
        require(kind in {"perpDexs", "clearinghouseState", "spotClearinghouseState", "frontendOpenOrders",
                         "meta", "metaAndAssetCtxs", "l2Book", "extraAgents", "orderStatus", "userFees",
                         "userFillsByTime", "userFunding", "userNonFundingLedgerUpdates", "userAbstraction", "activeAssetData"},
                "unsupported read")
        return self.post("info", {"type": kind, **fields})

    def exchange(self, signed):
        require(self.network == "testnet", "mainnet execution is not released")
        return self.post("exchange", signed)


def dex_fields(coin):
    return {"dex": coin.split(":", 1)[0]} if ":" in coin else {}


def positions(state):
    result = [p["position"] for p in state["assetPositions"] if number(p["position"]["szi"]) != 0]
    require(len({p["coin"] for p in result}) == len(result), "duplicate positions")
    return result


def identity(state):
    return sorted((p["coin"], p["szi"], p.get("entryPx"), json.dumps(p["leverage"], sort_keys=True))
                  for p in positions(state))


def snapshot(venue, account):
    address(account)
    start = now_ms()
    discovery = venue.info("perpDexs")
    from .discovery import names as discover_names, bootstrap
    names = discover_names(discovery)

    def observe(name):
        fields = {"dex": name} if name else {}
        before = venue.info("clearinghouseState", user=account, **fields)
        orders = venue.info("frontendOpenOrders", user=account, **fields)
        after = venue.info("clearinghouseState", user=account, **fields)
        require(identity(before) == identity(after), "position changed during order observation")
        require(isinstance(orders, list) and len({o["oid"] for o in orders}) == len(orders), "invalid orders")
        for p in positions(after):
            require(dex_fields(p["coin"]) == fields, "position DEX mismatch")
        return {"dex": name, "before": before, "state": after, "orders": orders}

    # Preserve sequencing within a DEX while bounding concurrent independent reads.
    if len(names) > 20:
        dexes = bootstrap(venue.network, account, names)
        # Complete fresh socket snapshots establish account scope. Every active
        # DEX (and native even when flat) must also pass independent REST reads.
        active = [d for d in dexes if d["dex"] == "" or positions(d["state"]) or d["orders"]]
        with ThreadPoolExecutor(max_workers=4) as pool:
            checked = list(pool.map(observe, [d["dex"] for d in active]))
        for streamed, rest in zip(active, checked):
            require(identity(streamed["state"]) == identity(rest["state"]), "stream/REST positions differ")
            require({o["oid"] for o in streamed["orders"]} == {o["oid"] for o in rest["orders"]},
                    "stream/REST orders differ")
            streamed.update(rest)
    else:
        with ThreadPoolExecutor(max_workers=4) as pool:
            dexes = list(pool.map(observe, names))
    balance = venue.info("spotClearinghouseState", user=account)
    require(discover_names(venue.info("perpDexs")) == names, "DEX discovery changed")
    completed = now_ms()
    require(completed - start <= 30_000, "snapshot exceeded freshness budget")
    for dex in dexes:
        for state in (dex["before"], dex["state"]):
            require(completed - 30_000 <= state["time"] <= completed + 5000, "stale position evidence")
    available = next((value for token, value in balance.get("tokenToAvailableAfterMaintenance", []) if token == 0), None)
    return {"schema_version": 1, "network": venue.network, "account": account,
            "started_at_ms": start, "completed_at_ms": completed, "dexs": dexes,
            "balance": balance, "available_usdc": available,
            "observation_method": "fresh_stream_bootstrap_and_active_rest" if len(names) > 20 else "all_dex_rest"}


def find_position(observation, coin):
    found = [p for d in observation["dexs"] for p in positions(d["state"]) if p["coin"] == coin]
    require(len(found) <= 1, "ambiguous position")
    return found[0] if found else None


def coin_orders(observation, coin):
    return [o for d in observation["dexs"] for o in d["orders"] if o["coin"] == coin]


def market(venue, coin):
    fields = dex_fields(coin)
    discovery = venue.info("perpDexs")
    index = 0 if not fields else next((i for i, d in enumerate(discovery) if d and d["name"] == fields["dex"]), None)
    require(index is not None, "unknown builder DEX")
    metadata = venue.info("meta", **fields)
    found = [(i, m) for i, m in enumerate(metadata["universe"]) if m["name"] == coin]
    require(len(found) == 1, "unknown market")
    offset, item = found[0]
    require(not item.get("isDelisted", False) and 0 <= item["szDecimals"] <= 6, "invalid/delisted market")
    return {**item, "asset": offset if index == 0 else 100_000 + index * 10_000 + offset}


def price_round(price, size_decimals, up):
    require(price.is_finite() and price > 0 and 0 <= size_decimals <= 6, "invalid price")
    # Perps: five significant figures, at most 6-szDecimals fractional places;
    # integer prices are always permitted.
    places = min(6 - size_decimals, max(0, 4 - price.adjusted()))
    return price.quantize(Decimal(1).scaleb(-places), rounding=ROUND_UP if up else ROUND_DOWN)


def close_quote(venue, coin, is_buy, quantity, size_decimals, slippage_bps):
    require(type(slippage_bps) is int and 0 <= slippage_bps <= 100, "slippage must be 0-100 bps")
    book = venue.info("l2Book", coin=coin)
    require(book["coin"] == coin and now_ms() - 5000 <= book["time"] <= now_ms() + 1000, "stale/wrong order book")
    levels = book["levels"][1 if is_buy else 0]
    require(levels, "empty book")
    best = number(levels[0]["px"])
    tolerance = Decimal(slippage_bps) / 10_000
    bound = price_round(best * (1 + tolerance if is_buy else 1 - tolerance), size_decimals, not is_buy)
    depth = sum((number(l["sz"]) for l in levels if (number(l["px"]) <= bound if is_buy else number(l["px"]) >= bound)), Decimal(0))
    require(depth >= quantity, "insufficient displayed depth inside price bound")
    return bound


def verify_agent(venue, p, expiry):
    execution_scope(venue, p)
    agents = venue.info("extraAgents", user=p["account"])
    require(any(a["address"].lower() == p["signer_address"] and a["validUntil"] >= expiry for a in agents),
            "agent is absent, revoked or expires before action")


def check_close_notional(quantity, limit, residual):
    """Partial IOC closes need 10 USDC; preserve the full-residual close path.

    Use the quantized quantity and approved limit, never a mark-price estimate.
    Callers also validate size, side, ownership and current exposure.
    """
    require(quantity == residual or quantity * limit >= Decimal("10"),
            "partial close must have at least 10 USDC notional at the approved limit")


def fresh_action_evidence(venue, p, action):
    """Final signed-action check. Never substitutes a cache for venue evidence."""
    state = venue.info("clearinghouseState", user=p["account"], **dex_fields(p["coin"]))
    require(now_ms() - 5000 <= state["time"] <= now_ms() + 1000, "stale final position read")
    current = next((v for v in positions(state) if v["coin"] == p["coin"]), None)
    if action["type"] in ("cancel", "updateLeverage"):
        require(current is None, "default cancel/configuration requires flat target")
        return
    require(action["type"] == "order", "unsupported final action")
    if any(not o["r"] for o in action["orders"]):
        require(current is None, "entry target changed before broadcast")
        orders = venue.info("frontendOpenOrders", user=p["account"], **dex_fields(p["coin"]))
        require(not any(o["coin"] == p["coin"] for o in orders), "entry target orders changed")
    else:
        require(current is not None and (number(current["szi"]) > 0) == p["is_buy"]
                and current["leverage"]["type"] == "isolated" and current["leverage"]["value"] == 1,
                "closing/protection exposure changed before broadcast")
        for order in action["orders"]:
            require(number(order["s"]) <= abs(number(current["szi"])), "action exceeds fresh residual")
            if "trigger" in order["t"]:
                require(number(order["s"]) == abs(number(current["szi"])), "protection must cover the fresh full position")
            elif "limit" in order["t"]:
                check_close_notional(number(order["s"]), number(order["p"]), abs(number(current["szi"])))
    book = venue.info("l2Book", coin=p["coin"])
    require(book["coin"] == p["coin"] and now_ms() - 5000 <= book["time"] <= now_ms() + 1000, "stale final book")
    for order in action["orders"]:
        if "limit" not in order["t"]:
            continue
        levels = book["levels"][1 if order["b"] else 0]
        depth = sum((number(level["sz"]) for level in levels
                     if (number(level["px"]) <= number(order["p"]) if order["b"] else number(level["px"]) >= number(order["p"]))), Decimal(0))
        require(depth >= number(order["s"]), "fresh depth no longer fits signed price")


def send(venue, policy, action, socket_path, directory, label, before_sign=None, before_submit=None):
    """Persist intent before signing; never retry an uncertain request."""
    execution_scope(venue, policy)
    now = now_ms()
    expiry = min(now + policy.get("action_ttl_ms", 10_000), policy["expires_at_ms"])
    require(expiry > now + 1000, "session expired")
    nonce_file = directory / "nonce.json"
    last = read(nonce_file)["nonce"] if nonce_file.exists() else 0
    require(type(last) is int and last <= now + 5000, "nonce clock regression exceeds bound")
    nonce = max(now, last + 1)
    if nonce > now:
        time.sleep((nonce - now) / 1000)
    verify_agent(venue, policy, expiry)
    request = {"schema_version": 1, "network": policy["network"], "key_id": policy["key_id"],
               "signer_address": policy["signer_address"], "nonce": nonce, "expires_after": expiry,
               "action": action}
    write(directory / (label + "-intent.json"), request)
    write(nonce_file, {"nonce": nonce}, replace=True)
    if before_sign is not None:
        before_sign(nonce)
    socket_stat = os.lstat(socket_path)
    require(stat.S_ISSOCK(socket_stat.st_mode) and socket_stat.st_uid == os.getuid()
            and socket_stat.st_mode & 0o077 == 0, "signer socket must be private and owned")
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(5)
        connection.connect(str(socket_path))
        connection.sendall(json.dumps(request).encode())
        connection.shutdown(socket.SHUT_WR)
        data = bytearray()
        while chunk := connection.recv(4096):
            data.extend(chunk)
            require(len(data) <= 65536, "oversized signer response")
    response = json.loads(data)
    require(response.get("schema_version") == 1 and response.get("network") == policy["network"]
            and response.get("key_id") == policy["key_id"] and response.get("signer_address") == policy["signer_address"],
            "signer response identity mismatch")
    signed = response["signed_request"]
    require(set(signed) == {"action", "nonce", "expiresAfter", "vaultAddress", "signature"}
            and signed["action"] == action and signed["nonce"] == nonce
            and signed["expiresAfter"] == expiry and signed["vaultAddress"] is None, "signer changed request")
    from hyperliquid.utils.signing import recover_agent_or_user_from_l1_action
    require(recover_agent_or_user_from_l1_action(wire_action(action), signed["signature"], None,
                                                nonce, expiry, policy["network"] == "mainnet").lower() == policy["signer_address"],
            "invalid signer signature")
    if before_submit is None:
        fresh_action_evidence(venue, policy, action)
    else:
        before_submit()
    verify_agent(venue, policy, expiry)
    require(now_ms() + 5000 <= expiry, "less than five seconds remain for venue receipt; reconcile without retry")
    write(directory / (label + "-broadcast.json"), {"nonce": nonce, "at_ms": now_ms()})
    result = venue.exchange(signed)
    write(directory / (label + "-response.json"), result)
    return result
