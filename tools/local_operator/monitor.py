"""Read-only collector using existing Rust archives and notification events."""
import json
from pathlib import Path
import subprocess
import threading
import time

from .entry import artifact
from .signer import now_ms
from .store import lock, private_dir, read, require, write


def observe(binary, network, account, archive):
    require(artifact(binary["path"]) == binary, "collector binary changed")
    result = subprocess.run([binary["path"], "account", "--network", network, "--address", account,
                             "--output", "json", "portfolio", "--require-exits", "--archive-dir", str(archive)],
                            text=True, capture_output=True, timeout=130, check=False)
    if result.returncode not in (0, 14) or not result.stdout.strip():
        return {"schema_version": 1, "network": network, "account": account,
                "observed_at_ms": now_ms(), "status": "unknown", "snapshot": None}
    value = json.loads(result.stdout)
    require(value["schema_version"] == 2 and value["network"] == network and value["account"].lower() == account,
            "collector response scope/schema mismatch")
    require(0 <= now_ms() - value["completed_at_ms"] <= 30_000, "collector response is stale")
    return {"schema_version": 1, "network": network, "account": account, "observed_at_ms": value["completed_at_ms"],
            "status": "protected" if result.returncode == 0 else "protection_missing", "snapshot": value}


def cached(directory, network, account, max_age_ms=30000):
    require(type(max_age_ms) is int and 0 < max_age_ms <= 3_600_000, "invalid cache age limit")
    value = read(Path(directory) / "latest.json")
    require(value["network"] == network and value["account"] == account, "cache identity mismatch")
    age = now_ms() - value["observed_at_ms"]
    return {**value, "age_ms": age, "usable": value["snapshot"] is not None and 0 <= age <= max_age_ms}


def websocket_wakeup(network, account, wake, stop):
    # Events only accelerate REST reads; they never authorize a trade or replace
    # an account snapshot. Periodic REST polling survives disconnects/gaps.
    import websocket
    endpoint = "wss://api.hyperliquid" + ("-testnet" if network == "testnet" else "") + ".xyz/ws"
    while not stop.is_set():
        connection = None
        try:
            connection = websocket.create_connection(endpoint, timeout=2)
            for kind in ("orderUpdates", "userFills"):
                connection.send(json.dumps({"method": "subscribe", "subscription": {"type": kind, "user": account}}))
            wake.set()  # Every reconnect requires reconciliation, including missing events.
            while not stop.is_set():
                try:
                    message = connection.recv()
                except websocket.WebSocketTimeoutException:
                    connection.ping()
                    continue
                require(len(message) <= 1_048_576, "oversized stream event")
                event = json.loads(message)
                if event.get("channel") in ("orderUpdates", "userFills"):
                    wake.set()
        except Exception:
            wake.set()
            stop.wait(5)
        finally:
            if connection is not None:
                connection.close()


def watch(binary_path, network, account, directory, interval=30, duration=3600, websocket=False, once=False,
          ledger_directory=None, ledger_start=None):
    require(network in ("mainnet", "testnet") and 30 <= interval <= 3600 and 1 <= duration <= 86400, "invalid monitor bounds")
    root = private_dir(directory)
    binary = artifact(binary_path)
    wake, stop = threading.Event(), threading.Event()
    worker = None
    with lock(root / "collector.lock"):
        binding = root / "collector-identity.json"
        identity = {"network": network, "account": account}
        if binding.exists():
            require(read(binding) == identity, "collector directory belongs to another account")
        else:
            write(binding, identity)
        if websocket:
            worker = threading.Thread(target=websocket_wakeup, args=(network, account, wake, stop), daemon=True)
            worker.start()
        deadline = time.monotonic() + duration
        try:
            while True:
                started = time.monotonic()
                wake.clear()
                try:
                    value = observe(binary, network, account, root)
                except Exception:
                    value = {"schema_version": 1, **identity, "observed_at_ms": now_ms(), "status": "unknown", "snapshot": None}
                write(root / "latest.json", value, replace=True)
                if ledger_directory is not None:
                    from .ledger import sync
                    from .venue import Venue
                    try:
                        ledger_path = Path(ledger_directory) / "ledger.json"
                        start = max(0, max(w["end_ms"] for w in read(ledger_path)["windows"]) - 60_000) if ledger_path.exists() else ledger_start
                        require(start is not None, "first collector ledger sync needs explicit start time")
                        sync(Venue(network), account, ledger_directory, start, now_ms())
                        ledger_status = "synced"
                    except Exception:
                        ledger_status = "unknown"
                    write(root / "ledger-health.json", {"observed_at_ms": now_ms(), "status": ledger_status}, replace=True)
                print(json.dumps({key: value[key] for key in ("status", "observed_at_ms")}), flush=True)
                if once or time.monotonic() >= deadline:
                    return value
                # At most two full reconciliations per minute, even under event storms.
                stop.wait(max(0, min(30 - (time.monotonic() - started), deadline - time.monotonic())))
                wake.wait(max(0, min(interval - (time.monotonic() - started), deadline - time.monotonic())))
                if time.monotonic() >= deadline:
                    return value
        finally:
            stop.set()
            if worker:
                worker.join(timeout=3)
