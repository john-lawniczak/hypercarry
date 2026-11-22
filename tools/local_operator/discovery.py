"""Fresh, complete all-DEX bootstrap for large discovery sets; never a cache.

Orders are full per-DEX subscriptions, not deltas. A second all-DEX position
snapshot brackets all initial order snapshots. Active DEXs are rechecked by REST
by the caller. Missing subscriptions, changed identities and deadlines fail closed.
"""
import json
import time

from .signer import now_ms
from .store import require


def names(discovery):
    require(isinstance(discovery, list) and 1 <= len(discovery) <= 512 and discovery[0] is None,
            "invalid DEX discovery (512-DEX budget)")
    result = [""] + [d["name"] for d in discovery[1:]]
    require(all(isinstance(n, str) and 0 < len(n) <= 64 and not any(ord(c) < 32 for c in n)
                for n in result[1:]) and len(set(result)) == len(result), "invalid/duplicate DEX name")
    return result


def decode_states(data, account, expected):
    require(data["user"].lower() == account, "stream account mismatch")
    rows = data["clearinghouseStates"]
    require(isinstance(rows, list) and all(isinstance(r, list) and len(r) == 2 for r in rows),
            "invalid all-DEX state")
    require(len(rows) == len(expected) and {r[0] for r in rows} == set(expected),
            "missing or duplicate DEX state")
    return dict(rows)


def bootstrap(network, account, expected, connect=None):
    from .venue import identity
    if connect is None:
        import websocket
        connect = websocket.create_connection
    endpoint = "wss://api.hyperliquid" + ("-testnet" if network == "testnet" else "") + ".xyz/ws"
    start = now_ms()
    deadline = time.monotonic() + 15
    connection = connect(endpoint, timeout=3)
    before, after, orders = None, None, {}
    refresh_requested = refresh_acknowledged = False
    try:
        connection.send(json.dumps({"method": "subscribe", "subscription": {
            "type": "allDexsClearinghouseState", "user": account}}))
        sent_orders = False
        for _ in range(10000):
            require(time.monotonic() < deadline, "all-DEX bootstrap deadline exceeded")
            message = connection.recv()
            require(len(message) <= 16 * 1024 * 1024, "oversized stream snapshot")
            event = json.loads(message)
            channel, data = event.get("channel"), event.get("data")
            if channel == "error":
                raise ValueError("stream subscription refused")
            if channel == "subscriptionResponse" and refresh_requested:
                if data.get("method") == "subscribe" and data.get("subscription", {}).get("type") == "allDexsClearinghouseState":
                    refresh_acknowledged = True
            if channel == "allDexsClearinghouseState":
                states = decode_states(data, account, expected)
                if before is None:
                    before = states
                elif refresh_acknowledged:
                    after = states
                    break
            elif channel == "openOrders":
                require(data["user"].lower() == account and data["dex"] in expected,
                        "unexpected order subscription")
                rows = data["orders"]
                require(isinstance(rows, list) and len({r["oid"] for r in rows}) == len(rows),
                        "invalid order snapshot")
                orders[data["dex"]] = rows
            if before is not None and not sent_orders:
                for dex in expected:
                    connection.send(json.dumps({"method": "subscribe", "subscription": {
                        "type": "openOrders", "user": account, "dex": dex}}))
                sent_orders = True
            if len(orders) == len(expected) and not refresh_requested:
                # Flat accounts need not generate another update. A new subscription
                # explicitly requests a new full snapshot after all order snapshots.
                for method in ("unsubscribe", "subscribe"):
                    connection.send(json.dumps({"method": method, "subscription": {
                        "type": "allDexsClearinghouseState", "user": account}}))
                refresh_requested = True
        require(after is not None and len(orders) == len(expected), "incomplete all-DEX bootstrap")
    finally:
        connection.close()
    completed = now_ms()
    for dex in expected:
        require(identity(before[dex]) == identity(after[dex]), "position changed during order bootstrap")
        for state in (before[dex], after[dex]):
            require(start - 5000 <= state["time"] <= completed + 1000, "stale all-DEX snapshot")
    return [{"dex": dex, "before": before[dex], "state": after[dex], "orders": orders[dex]} for dex in expected]
