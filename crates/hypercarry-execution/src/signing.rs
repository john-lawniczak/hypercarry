use crate::{ExecutionError, ExecutionNetwork, Signer};

/// Validates that an isolated signing provider selected the exact requested
/// network and exposes a non-empty, non-secret audit alias.
///
/// # Errors
///
/// Returns an error for a network mismatch or unsafe/empty key identifier.
pub fn validate_signer<S: Signer>(
    signer: &S,
    expected_network: ExecutionNetwork,
) -> Result<(), ExecutionError> {
    if signer.network() != expected_network {
        return Err(ExecutionError::Policy(format!(
            "signer network {} does not match requested {expected_network}",
            signer.network()
        )));
    }
    let key_id = signer.key_id();
    if key_id.is_empty()
        || key_id.len() > 128
        || !key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
    {
        return Err(ExecutionError::Policy(
            "signer key ID must be a bounded non-secret provider alias".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    struct Provider {
        network: ExecutionNetwork,
        key_id: &'static str,
    }

    impl Signer for Provider {
        type Error = Infallible;

        fn network(&self) -> ExecutionNetwork {
            self.network
        }

        fn key_id(&self) -> &str {
            self.key_id
        }

        fn sign(&self, _payload: &[u8]) -> Result<Vec<u8>, Self::Error> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn network_scoped_key_selection_is_enforced() {
        let provider = Provider {
            network: ExecutionNetwork::Testnet,
            key_id: "hsm/hypercarry-testnet",
        };
        assert!(validate_signer(&provider, ExecutionNetwork::Testnet).is_ok());
        assert!(validate_signer(&provider, ExecutionNetwork::Mainnet).is_err());
    }

    #[test]
    fn key_alias_cannot_smuggle_secret_like_text() {
        let provider = Provider {
            network: ExecutionNetwork::Testnet,
            key_id: "0xabc def",
        };
        assert!(validate_signer(&provider, ExecutionNetwork::Testnet).is_err());
    }
}
