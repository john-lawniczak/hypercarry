use hypercarry_signing::{
    Chain, DigestSigner, SigningKey,
    bracket::{BracketAction, BracketRequest},
};
use serde_json::Value;

#[test]
fn official_sdk_bracket_hashes_and_signatures_match() {
    let vectors: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/bracket-vectors.json")).unwrap();
    assert_eq!(vectors.len(), 14);
    let mut bytes = [0; 32];
    bytes[31] = 1; // Public unfunded fixture; never a credential.
    let signer = SigningKey::from_slice(&bytes).unwrap();
    for vector in vectors {
        let expected: BracketRequest = serde_json::from_value(vector["signed"].clone()).unwrap();
        let chain = if vector["mainnet"] == true {
            Chain::Mainnet
        } else {
            Chain::Testnet
        };
        let action: &BracketAction = &expected.action;
        assert_eq!(
            action
                .prehash(expected.nonce, expected.expires_after, chain)
                .unwrap()
                .to_string(),
            vector["hash"]
        );
        let actual = action
            .sign(&signer, expected.nonce, expected.expires_after, chain)
            .unwrap();
        assert_eq!(actual.signature.r, expected.signature.r);
        assert_eq!(actual.signature.s, expected.signature.s);
        assert_eq!(actual.signature.v, expected.signature.v);
        assert_eq!(actual.recover(chain).unwrap(), signer.address());
        let wrong = if vector["mainnet"] == true {
            Chain::Testnet
        } else {
            Chain::Mainnet
        };
        assert_ne!(actual.recover(wrong).unwrap(), signer.address());
        let mut changed = actual.clone();
        changed.nonce += 1;
        assert_ne!(changed.recover(chain).unwrap(), signer.address());
        let mut changed = actual;
        changed.expires_after += 1;
        assert_ne!(changed.recover(chain).unwrap(), signer.address());
    }
}
