"""Offline tests using only the deliberately public, unfunded scalar-one key."""
import json
from pathlib import Path
import unittest
import os
import socket
import tempfile
import threading
from unittest.mock import patch

from .signer import wire_action
from .signer import Session, now_ms, receive
from .store import lock
from .test_signer import policy, order
from .test_workflow import FakeVenue
from .venue import send

try:
    from eth_account import Account
    from hyperliquid.utils.signing import sign_l1_action
except ImportError:
    Account = None


@unittest.skipIf(Account is None, "run with pinned SDK to exercise signature compatibility")
class SdkTests(unittest.TestCase):
    def test_signer_matches_existing_rust_bracket_vectors_after_sorted_storage(self):
        root = Path(__file__).resolve().parents[2]
        vectors = json.loads((root / "crates/hypercarry-signing/tests/fixtures/bracket-vectors.json").read_text())
        wallet = Account.from_key(bytes.fromhex("00" * 31 + "01"))
        checked = 0
        for vector in vectors:
            signed = vector["signed"]
            if vector["mainnet"] or signed["action"]["type"] == "updateIsolatedMargin":
                continue
            action = wire_action(json.loads(json.dumps(signed["action"], sort_keys=True)))
            signature = sign_l1_action(wallet, action, None, signed["nonce"], signed["expiresAfter"], False)
            for scalar in ("r", "s"):
                self.assertEqual(int(signature[scalar], 16), int(signed["signature"][scalar], 16))
            self.assertEqual(signature["v"], signed["signature"]["v"])
            checked += 1
        self.assertGreaterEqual(checked, 4)

    def test_recovery_cloid_cancellation_matches_rust_vectors(self):
        root = Path(__file__).resolve().parents[2]
        vectors = json.loads((root / "crates/hypercarry-signing/tests/fixtures/legacy-vectors.json").read_text())
        wallet = Account.from_key(bytes.fromhex("00" * 31 + "01"))
        checked = 0
        for vector in vectors:
            signed = vector["signed"]
            if vector["mainnet"] or signed["action"]["type"] != "cancelByCloid":
                continue
            action = wire_action(json.loads(json.dumps(signed["action"], sort_keys=True)))
            signature = sign_l1_action(wallet, action, None, signed["nonce"], signed.get("expiresAfter"), False)
            for scalar in ("r", "s"):
                self.assertEqual(int(signature[scalar], 16), int(signed["signature"][scalar], 16))
            self.assertEqual(signature["v"], signed["signature"]["v"])
            checked += 1
        self.assertEqual(checked, 2)

    def test_encrypted_fixture_unlock_pins_agent_identity(self):
        key = bytes.fromhex("00" * 31 + "01")
        encrypted = Account.encrypt(key, "public-fixture-only", kdf="pbkdf2", iterations=1000)
        self.assertEqual(Account.decrypt(encrypted, "public-fixture-only"), key)
        with self.assertRaises(ValueError):
            Account.decrypt(encrypted, "incorrect-fixture-password")

    def test_real_socket_signature_verification_and_no_signature_persistence(self):
        self.socket_case(False)

    def test_lost_exchange_ack_preserves_unsigned_intent_without_retry(self):
        self.socket_case(True)

    def test_wrong_recovered_signature_never_reaches_exchange(self):
        self.socket_case(False, "signature")

    def test_position_changes_during_signing_never_reach_exchange(self):
        self.socket_case(False, "resize")

    def test_agent_revocation_during_signing_never_reaches_exchange(self):
        self.socket_case(False, "revoked")

    def test_nearly_expired_signature_never_reaches_exchange(self):
        self.socket_case(False, "deadline")

    def test_explicit_thirty_second_policy_survives_slow_final_checks(self):
        self.socket_case(False, action_ttl_ms=30000)

    def test_full_close_below_minimum_survives_signing_and_final_checks(self):
        self.socket_case(False, close_quantity="0.05")

    def test_growth_during_signing_revokes_subminimum_full_close_exception(self):
        self.socket_case(False, "minimum", close_quantity="0.05")

    def socket_case(self, lose_ack, failure=None, action_ttl_ms=None, close_quantity="0.1"):
        wallet = Account.from_key(bytes.fromhex("00" * 31 + "01"))
        p = policy()
        p.update(signer_address=wallet.address.lower(), expires_at_ms=now_ms()+60000,
                 allow_entry=False, quantity=close_quantity)
        if action_ttl_ms is not None:
            p["action_ttl_ms"] = action_ttl_ms
        action = {"type": "order", "orders": [order(50, size=close_quantity)], "grouping": "na"}
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            signer_root = root / "signer"
            session = Session(p, signer_root)
            path = signer_root / "agent.sock"
            ready, errors = threading.Event(), []
            def backend():
                try:
                    with lock(signer_root / "agent.lock"), socket.socket(socket.AF_UNIX) as listener:
                        listener.bind(str(path))
                        os.chmod(path, 0o600)
                        listener.listen(1)
                        listener.settimeout(5)
                        ready.set()
                        connection, _ = listener.accept()
                        with connection:
                            request = receive(connection)
                            session.reserve(request, now_ms())
                            wire = wire_action(request["action"])
                            signing_wallet = Account.from_key(bytes.fromhex("00" * 31 + "02")) if failure == "signature" else wallet
                            signature = sign_l1_action(signing_wallet, wire, None, request["nonce"], request["expires_after"], False)
                            response = {"schema_version": 1, "network": "testnet", "key_id": p["key_id"],
                                        "signer_address": p["signer_address"], "signed_request": {
                                            "action": wire, "nonce": request["nonce"], "expiresAfter": request["expires_after"],
                                            "vaultAddress": None, "signature": signature}}
                            connection.sendall(json.dumps(response).encode())
                except Exception as error:
                    errors.append(type(error).__name__)
                    ready.set()
            thread = threading.Thread(target=backend)
            thread.start()
            self.assertTrue(ready.wait(5))
            venue = FakeVenue()
            venue.position["szi"] = "-" + close_quantity
            info = venue.info
            queries = []
            elapsed = [0]
            def query(kind, **fields):
                if kind == "extraAgents":
                    queries.append(kind)
                    if failure == "revoked" and len(queries) > 1:
                        return []
                    if len(queries) > 1 and (failure == "deadline" or action_ttl_ms is not None):
                        elapsed[0] = 9782
                    return [{"address": p["signer_address"], "validUntil": p["expires_at_ms"]}]
                if failure == "resize" and kind == "clearinghouseState":
                    venue.position["szi"] = "-0.01"
                if failure == "minimum" and kind == "clearinghouseState":
                    venue.position["szi"] = "-0.1"
                return info(kind, **fields)
            exchange_calls = []
            def exchange(signed):
                exchange_calls.append(signed)
                if lose_ack:
                    raise TimeoutError()
                return {"status": "ok"}
            try:
                with patch.object(venue, "info", side_effect=query), patch.object(venue, "exchange", side_effect=exchange, create=True), \
                        patch("tools.local_operator.venue.now_ms", side_effect=lambda: now_ms() + elapsed[0]):
                    if failure:
                        with self.assertRaises(ValueError) as refused:
                            send(venue, p, action, path, root, "fixture")
                        if failure == "minimum":
                            self.assertIn("at least 10 USDC", str(refused.exception))
                    elif lose_ack:
                        with self.assertRaises(TimeoutError):
                            send(venue, p, action, path, root, "fixture")
                    else:
                        self.assertEqual(send(venue, p, action, path, root, "fixture"), {"status": "ok"})
            finally:
                thread.join(timeout=6)
            self.assertFalse(thread.is_alive())
            self.assertEqual(errors, [])
            self.assertEqual(len(exchange_calls), 0 if failure else 1)
            self.assertTrue((root / "fixture-intent.json").exists())
            self.assertEqual((root / "fixture-broadcast.json").exists(), not failure)
            self.assertEqual((root / "fixture-response.json").exists(), not lose_ack and not failure)
            for file in root.rglob("*.json"):
                self.assertNotIn('"signature"', file.read_text())
