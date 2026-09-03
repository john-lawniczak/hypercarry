use hypercarry_execution::{ExecutionNetwork, HyperliquidL1Signer, HyperliquidSigningRequest};
#[cfg(unix)]
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::UnixStream,
    },
};

const SIGNER_PROTOCOL_VERSION: u32 = 1;
#[cfg(unix)]
const MAX_SIGNER_RESPONSE_BYTES: u64 = 64 * 1024;

/// Testnet signer accessed through a private, local Unix-domain socket.
///
/// The operator never starts a signer process, inherits its environment, or
/// loads key material. The separately reviewed provider owns the socket and
/// returns one complete Hyperliquid signed request.
pub(crate) struct UnixSocketTestnetSigner {
    path: PathBuf,
    key_id: String,
    signer_address: String,
    timeout: Duration,
}

impl UnixSocketTestnetSigner {
    pub(crate) fn new(
        path: PathBuf,
        key_id: String,
        signer_address: String,
        timeout: Duration,
    ) -> Result<Self, SignerError> {
        validate_socket(&path)?;
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(SignerError::Configuration);
        }
        Ok(Self {
            path,
            key_id,
            signer_address,
            timeout,
        })
    }

    #[cfg(unix)]
    fn exchange(&self, request: &SignerRequest<'_>) -> Result<Value, SignerError> {
        validate_socket(&self.path)?;
        let mut stream = UnixStream::connect(&self.path).map_err(|_| SignerError::Unavailable)?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|_| SignerError::Unavailable)?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|_| SignerError::Unavailable)?;

        serde_json::to_writer(&mut stream, request).map_err(|_| SignerError::Protocol)?;
        stream
            .write_all(b"\n")
            .map_err(|_| SignerError::Unavailable)?;
        stream.flush().map_err(|_| SignerError::Unavailable)?;
        stream
            .shutdown(std::net::Shutdown::Write)
            .map_err(|_| SignerError::Unavailable)?;

        let mut response = Vec::new();
        stream
            .take(MAX_SIGNER_RESPONSE_BYTES + 1)
            .read_to_end(&mut response)
            .map_err(|_| SignerError::Unavailable)?;
        self.validate_response(&response)
    }

    #[cfg(not(unix))]
    fn exchange(&self, _request: &SignerRequest<'_>) -> Result<Value, SignerError> {
        Err(SignerError::Configuration)
    }

    #[cfg(unix)]
    fn validate_response(&self, response: &[u8]) -> Result<Value, SignerError> {
        if response.is_empty() || response.len() as u64 > MAX_SIGNER_RESPONSE_BYTES {
            return Err(SignerError::Protocol);
        }
        let response: SignerResponse =
            serde_json::from_slice(response).map_err(|_| SignerError::Protocol)?;
        if response.schema_version != SIGNER_PROTOCOL_VERSION
            || response.network != ExecutionNetwork::Testnet
            || response.key_id != self.key_id
            || response.signer_address != self.signer_address
        {
            return Err(SignerError::IdentityMismatch);
        }
        Ok(response.signed_request)
    }
}

impl HyperliquidL1Signer for UnixSocketTestnetSigner {
    type Error = SignerError;

    fn network(&self) -> ExecutionNetwork {
        ExecutionNetwork::Testnet
    }

    fn key_id(&self) -> &str {
        &self.key_id
    }

    fn signer_address(&self) -> &str {
        &self.signer_address
    }

    fn sign_l1_action(&self, request: &HyperliquidSigningRequest) -> Result<Value, Self::Error> {
        self.exchange(&SignerRequest {
            schema_version: SIGNER_PROTOCOL_VERSION,
            network: request.network(),
            key_id: &self.key_id,
            signer_address: &self.signer_address,
            nonce: request.nonce(),
            expires_after: request.expires_after(),
            action: request.action(),
        })
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct SignerRequest<'a> {
    schema_version: u32,
    network: ExecutionNetwork,
    key_id: &'a str,
    signer_address: &'a str,
    nonce: u64,
    expires_after: u64,
    action: &'a Value,
}

#[cfg(unix)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignerResponse {
    schema_version: u32,
    network: ExecutionNetwork,
    key_id: String,
    signer_address: String,
    signed_request: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignerError {
    Configuration,
    Unavailable,
    Protocol,
    IdentityMismatch,
}

impl fmt::Display for SignerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "external signer configuration is invalid",
            Self::Unavailable => "external signer is unavailable",
            Self::Protocol => "external signer returned an invalid bounded response",
            Self::IdentityMismatch => "external signer identity does not match pinned config",
        })
    }
}

impl Error for SignerError {}

#[cfg(unix)]
pub(crate) fn validate_socket(path: &Path) -> Result<(), SignerError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| SignerError::Unavailable)?;
    let parent = path.parent().ok_or(SignerError::Configuration)?;
    validate_private_parent(parent)?;
    if !metadata.file_type().is_socket() || metadata.file_type().is_symlink() {
        return Err(SignerError::Configuration);
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn validate_socket(_path: &Path) -> Result<(), SignerError> {
    Err(SignerError::Configuration)
}

#[cfg(unix)]
fn validate_private_parent(parent: &Path) -> Result<(), SignerError> {
    let metadata = fs::symlink_metadata(parent).map_err(|_| SignerError::Unavailable)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(SignerError::Configuration);
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    const ADDRESS: &str = "0x1111111111111111111111111111111111111111";

    #[test]
    fn signer_response_pins_identity_without_retaining_secret_material() {
        let signer = fixture_signer();
        let response = serde_json::to_vec(&json!({
            "schema_version": 1,
            "network": "testnet",
            "key_id": "keychain/testnet-agent",
            "signer_address": ADDRESS,
            "signed_request": {"signed": true}
        }))
        .unwrap();
        let signed_request = signer.validate_response(&response).unwrap();
        assert_eq!(signed_request, json!({"signed": true}));

        let mut wrong_identity: Value = serde_json::from_slice(&response).unwrap();
        wrong_identity["signer_address"] =
            Value::String("0x2222222222222222222222222222222222222222".to_owned());
        assert_eq!(
            signer.validate_response(&serde_json::to_vec(&wrong_identity).unwrap()),
            Err(SignerError::IdentityMismatch)
        );
    }

    #[test]
    fn socket_parent_must_not_be_accessible_to_other_users() {
        let directory = tempdir().unwrap();
        let insecure = directory.path().join("insecure");
        fs::create_dir(&insecure).unwrap();
        fs::set_permissions(&insecure, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            validate_private_parent(&insecure),
            Err(SignerError::Configuration)
        );
    }

    fn fixture_signer() -> UnixSocketTestnetSigner {
        UnixSocketTestnetSigner {
            path: PathBuf::from("/unused/testnet-signer.sock"),
            key_id: "keychain/testnet-agent".to_owned(),
            signer_address: ADDRESS.to_owned(),
            timeout: Duration::from_secs(1),
        }
    }
}
