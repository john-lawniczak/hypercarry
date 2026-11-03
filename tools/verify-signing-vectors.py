"""Verify public, unfunded signing fixtures against the official Python SDK.

Run with the pinned SDK version documented in docs/signing-migration-review.md.
No RPC calls, credentials, or account operations are used.
"""

import importlib.metadata
import json
from pathlib import Path

from eth_account import Account
from eth_account.messages import _hash_eip191_message, encode_typed_data
from hyperliquid.utils.signing import (
    action_hash,
    construct_phantom_agent,
    l1_payload,
    sign_l1_action,
)


def require_equal(actual, expected, description):
    # Unlike assert, this remains active under python -O.
    if actual != expected:
        raise ValueError(f"Signing fixture mismatch: {description}")


def wire_action(action):
    # SDK serialization order, not the sorted JSON fixture's object order.
    kind = action["type"]
    if kind == "order":
        keys = ["a", "b", "p", "s", "r", "t", "c"]
        return dict(
            type=kind,
            orders=[{key: order[key] for key in keys if key in order}
                    for order in action["orders"]],
            grouping=action["grouping"],
        )
    if kind in ["cancel", "cancelByCloid"]:
        keys = ["a", "o"] if kind == "cancel" else ["asset", "cloid"]
        return dict(
            type=kind,
            cancels=[{key: cancel[key] for key in keys}
                     for cancel in action["cancels"]],
        )
    if kind == "scheduleCancel":
        return dict(type=kind, time=action["time"])
    raise ValueError(f"Unsupported fixture action: {kind}")


def main():
    root = Path(__file__).resolve().parent.parent
    fixture = root / "crates/hypercarry-signing/tests/fixtures/legacy-vectors.json"
    vectors = json.loads(fixture.read_text())
    require_equal(len(vectors), 28, "fixture count")
    wallet = Account.from_key(bytes.fromhex("00" * 31 + "01"))
    for index, vector in enumerate(vectors):
        signed = vector["signed"]
        action = wire_action(signed["action"])
        connection = action_hash(action, None, signed["nonce"], signed["expiresAfter"])
        agent = construct_phantom_agent(connection, vector["mainnet"])
        digest = _hash_eip191_message(encode_typed_data(full_message=l1_payload(agent)))
        require_equal(digest.hex(), vector["hash"][2:], f"{index}: digest")
        signature = sign_l1_action(
            wallet, action, None, signed["nonce"], signed["expiresAfter"], vector["mainnet"]
        )
        for scalar in ["r", "s"]:
            require_equal(
                int(signature[scalar], 16), int(signed["signature"][scalar], 16),
                f"{index}: {scalar}",
            )
        require_equal(signature["v"], signed["signature"]["v"], f"{index}: parity")
    version = importlib.metadata.version("hyperliquid-python-sdk")
    print(f"Verified {len(vectors)} hashes and signatures with official hyperliquid-python-sdk {version}")


if __name__ == "__main__":
    main()
