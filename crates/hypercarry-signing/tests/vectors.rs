use hypercarry_signing::{
    Action, ActionRequest, Address, B256, Chain, DigestSigner, Signature, SignatureRecovery,
    SigningKey, U256,
};
use serde_json::{Value, json};

fn key() -> SigningKey {
    let mut bytes = [0; 32];
    bytes[31] = 1; // Published, unfunded test fixture, never a credential.
    SigningKey::from_slice(&bytes).unwrap()
}
fn vectors() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/legacy-vectors.json")).unwrap()
}
#[test]
fn legacy_sdk_hashes_payloads_and_signatures_match() {
    let signer = key();
    for vector in vectors() {
        let action: Action = serde_json::from_value(vector["input"].clone()).unwrap();
        let expected: ActionRequest = serde_json::from_value(vector["signed"].clone()).unwrap();
        let chain = if vector["mainnet"] == true {
            Chain::Mainnet
        } else {
            Chain::Testnet
        };
        let expiry = expected
            .expires_after
            .map(|e| chrono::DateTime::from_timestamp_millis(i64::try_from(e).unwrap()).unwrap());
        let hash = action.prehash(expected.nonce, None, expiry, chain).unwrap();
        assert_eq!(hash.to_string(), vector["hash"]);
        let actual = action
            .sign_sync(&signer, expected.nonce, None, expiry, chain)
            .unwrap();
        assert_eq!(serde_json::to_value(&actual).unwrap(), vector["signed"]);
        assert_eq!(actual.recover(chain).unwrap(), signer.address());
    }
}
#[test]
fn every_signed_context_component_is_bound() {
    for vector in vectors() {
        let request: ActionRequest = serde_json::from_value(vector["signed"].clone()).unwrap();
        let chain = if vector["mainnet"] == true {
            Chain::Mainnet
        } else {
            Chain::Testnet
        };
        let wrong_chain = if vector["mainnet"] == true {
            Chain::Testnet
        } else {
            Chain::Mainnet
        };
        let identity = key().address();
        assert_ne!(request.recover(wrong_chain).unwrap(), identity);
        let mut altered = request.clone();
        altered.nonce += 1;
        assert_ne!(altered.recover(chain).unwrap(), identity);
        let mut altered = request.clone();
        altered.expires_after = Some(request.expires_after.unwrap_or(1) + 1);
        assert_ne!(altered.recover(chain).unwrap(), identity);
        let mut altered = request.clone();
        altered.action =
            serde_json::from_value(json!({"type":"scheduleCancel","time":123})).unwrap();
        assert_ne!(altered.recover(chain).unwrap(), identity);
        let mut altered = request.clone();
        altered.vault_address = Some(Address::ZERO);
        assert!(altered.recover(chain).is_err());
        for v in [0, 1, 26, 29, 35, 255] {
            let mut altered = request.clone();
            altered.signature.v = v;
            assert!(altered.recover(chain).is_err());
        }
    }
}
#[test]
fn reject_invalid_scalars_and_high_s() {
    let hash = B256::repeat_byte(42);
    for sig in [
        Signature::new(U256::ZERO, U256::from(1), false),
        Signature::new(U256::MAX, U256::from(1), false),
        Signature::new(U256::from(1), U256::ZERO, false),
        Signature::new(U256::from(1), U256::MAX, false),
    ] {
        assert!(sig.recover_address_from_prehash(&hash).is_err());
    }
    let sig = key().sign_hash_sync(&hash).unwrap();
    let order = U256::from_str_radix(
        "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141",
        16,
    )
    .unwrap();
    let high = Signature::new(sig.r(), order - sig.s(), !sig.v());
    assert!(high.recover_address_from_prehash(&hash).is_err());
    assert_eq!(
        sig.recover_address_from_prehash(&hash).unwrap(),
        key().address()
    );
}
#[test]
fn reject_actions_outside_the_supported_surface() {
    for action in [
        json!({"type":"usdSend","amount":"1"}),
        json!({"type":"updateLeverage","asset":0,"leverage":2,"isCross":true}),
        json!({"type":"cancel","cancels":[],"f":true}),
        json!({"type":"scheduleCancel","time":1,"unexpected":true}),
    ] {
        assert!(serde_json::from_value::<Action>(action).is_err());
    }
    let base = vectors()[0]["input"].clone();
    for (path, value) in [
        (
            "builder",
            json!({"b":"0x0000000000000000000000000000000000000000","f":1}),
        ),
        ("grouping", json!("normalTpsl")),
    ] {
        let mut action = base.clone();
        action[path] = value;
        assert!(serde_json::from_value::<Action>(action).is_err());
    }
    for (field, value) in [
        ("a", json!(-1)),
        ("p", json!(1)),
        ("s", json!("NaN")),
        ("c", json!("0x1234")),
        ("extra", json!(true)),
        (
            "t",
            json!({"trigger":{"isMarket":true,"triggerPx":"1","tpsl":"sl"}}),
        ),
    ] {
        let mut action = base.clone();
        action["orders"][0][field] = value;
        assert!(serde_json::from_value::<Action>(action).is_err());
    }
}
#[test]
fn zero_cloid_omission_and_decimal_normalization_are_stable() {
    let mut input = vectors()[0]["input"].clone();
    input["orders"][0]["c"] = json!("0x00000000000000000000000000000000");
    let action: Action = serde_json::from_value(input).unwrap();
    let normalized = serde_json::to_value(&action).unwrap();
    assert!(normalized["orders"][0].get("c").is_none());
    assert_eq!(normalized["orders"][0]["p"], "2000");
    assert_eq!(normalized["orders"][0]["s"], "0.01");
}
#[test]
fn out_of_range_expiry_is_rejected_without_wrapping() {
    let mut request: ActionRequest =
        serde_json::from_value(vectors()[0]["signed"].clone()).unwrap();
    request.expires_after = Some(u64::MAX);
    assert!(request.recover(Chain::Testnet).is_err());
    assert!(
        request
            .action
            .prehash(
                1,
                None,
                chrono::DateTime::from_timestamp_millis(-1),
                Chain::Testnet
            )
            .is_err()
    );
}
