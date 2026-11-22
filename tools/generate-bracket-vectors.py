"""Regenerate PUBLIC, UNFUNDED bracket fixtures using official SDK 0.24.0.

No requests are sent. Uses the published private scalar 1 solely as a fixture.
Run with: uv run --with hyperliquid-python-sdk==0.24.0 python tools/generate-bracket-vectors.py
"""
import importlib.metadata
import json
from pathlib import Path
from eth_account import Account
from eth_account.messages import _hash_eip191_message, encode_typed_data
from hyperliquid.utils.signing import (
    action_hash, construct_phantom_agent, l1_payload, sign_l1_action,
    order_request_to_order_wire, order_wires_to_order_action,
)
from hyperliquid.utils.types import Cloid


def fixtures():
    def order(buy, price, reduce, kind, identity):
        return order_request_to_order_wire({
            "coin": "EXAMPLE", "is_buy": buy, "sz": 1.25,
            "limit_px": price, "order_type": kind, "reduce_only": reduce,
            "cloid": Cloid.from_str("0x" + identity * 32),
        }, 2)

    entry = order(False, 81.59, False, {"limit": {"tif": "Ioc"}}, "1")
    tp = order(True, 77, True, {"trigger": {"isMarket": True, "triggerPx": 70, "tpsl": "tp"}}, "2")
    sl = order(True, 104.5, True, {"trigger": {"isMarket": True, "triggerPx": 95, "tpsl": "sl"}}, "3")
    close = order(True, 83, True, {"limit": {"tif": "Ioc"}}, "4")
    return [
        {"type": "updateLeverage", "asset": 2, "isCross": False, "leverage": 1},
        {"type": "updateIsolatedMargin", "asset": 2, "isBuy": True, "ntli": 1000000},
        {"type": "updateIsolatedMargin", "asset": 2, "isBuy": True, "ntli": -1000000},
        order_wires_to_order_action([entry, tp, sl], grouping="normalTpsl"),
        order_wires_to_order_action([tp, sl], grouping="positionTpsl"),
        order_wires_to_order_action([close]),
        {"type": "cancel", "cancels": [{"a": 2, "o": 123}, {"a": 2, "o": 124}]},
    ]


def main():
    if importlib.metadata.version("hyperliquid-python-sdk") != "0.24.0":
        raise ValueError("Use the reviewed SDK pin 0.24.0")
    wallet = Account.from_key(bytes.fromhex("00" * 31 + "01"))
    vectors = []
    for mainnet in [False, True]:
        for action in fixtures():
            nonce, expiry = 1700000000000, 1700000030000
            connection = action_hash(action, None, nonce, expiry)
            agent = construct_phantom_agent(connection, mainnet)
            digest = _hash_eip191_message(encode_typed_data(full_message=l1_payload(agent)))
            vectors.append({"mainnet": mainnet, "hash": "0x" + digest.hex(), "signed": {
                "action": action, "nonce": nonce, "expiresAfter": expiry,
                "vaultAddress": None,
                "signature": sign_l1_action(wallet, action, None, nonce, expiry, mainnet),
            }})
    path = Path(__file__).resolve().parent.parent / "crates/hypercarry-signing/tests/fixtures/bracket-vectors.json"
    path.write_text(json.dumps(vectors, indent=2) + "\n")
    print(f"Wrote {len(vectors)} official SDK bracket fixtures")


if __name__ == "__main__":
    main()
