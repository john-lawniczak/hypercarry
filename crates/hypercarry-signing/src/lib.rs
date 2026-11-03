//! Narrow Hyperliquid L1 signing boundary. No network access or key loading.
//! Wire order is consensus-sensitive: do not hash a generic JSON map.
pub use alloy_primitives::{Address, B256, Signature, U256, keccak256};
use alloy_sol_types::{SolStruct, eip712_domain, sol};
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
pub use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};

/// Signing domain, independent of Ethereum's EIP-712 chain ID.
#[derive(Clone, Copy, Debug)]
pub enum Chain {
    Mainnet,
    Testnet,
}

/// Only the actions supported by the execution adapter are representable.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Action {
    Order {
        orders: Vec<Order>,
        grouping: Grouping,
    },
    Cancel {
        cancels: Vec<Cancel>,
    },
    CancelByCloid {
        cancels: Vec<CancelByCloid>,
    },
    ScheduleCancel {
        time: Option<u64>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Grouping {
    #[serde(rename = "na")]
    Na,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Order {
    a: u32,
    b: bool,
    #[serde(with = "decimal")]
    p: rust_decimal::Decimal,
    #[serde(with = "decimal")]
    s: rust_decimal::Decimal,
    r: bool,
    t: OrderType,
    #[serde(default, skip_serializing_if = "zero_cloid", with = "optional_cloid")]
    c: Option<alloy_primitives::B128>,
}
#[allow(clippy::ref_option)] // Required by serde skip_serializing_if.
fn zero_cloid(value: &Option<alloy_primitives::B128>) -> bool {
    value.is_none_or(|v| v.is_zero())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
enum OrderType {
    Limit { tif: TimeInForce },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
enum TimeInForce {
    Gtc,
    Ioc,
    Alo,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    a: u32,
    o: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelByCloid {
    asset: u32,
    #[serde(with = "cloid")]
    cloid: alloy_primitives::B128,
}
mod decimal {
    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(value: &Decimal, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.normalize().to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Decimal, D::Error> {
        String::deserialize(d)?
            .parse::<Decimal>()
            .map_err(D::Error::custom)
    }
}

/// Wire signature; accept only Ethereum recovery parity 27 or 28.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireSignature {
    pub r: U256,
    pub s: U256,
    pub v: u8,
}
impl From<Signature> for WireSignature {
    fn from(s: Signature) -> Self {
        Self {
            r: s.r(),
            s: s.s(),
            v: s.v_byte(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionRequest {
    pub action: Action,
    pub nonce: u64,
    pub signature: WireSignature,
    pub vault_address: Option<Address>,
    pub expires_after: Option<u64>,
}
impl ActionRequest {
    /// Recover this request's signing identity.
    /// # Errors
    /// Rejects unsupported vaults, invalid expiry, or invalid signatures.
    pub fn recover(&self, chain: Chain) -> Result<Address> {
        let expiry = self
            .expires_after
            .map(|v| {
                DateTime::from_timestamp_millis(i64::try_from(v)?)
                    .ok_or_else(|| anyhow::anyhow!("invalid expiry"))
            })
            .transpose()?;
        self.action.recover(
            &self.signature,
            self.nonce,
            self.vault_address,
            expiry,
            chain,
        )
    }
}
sol! { struct Agent { string source; bytes32 connectionId; } }
impl Action {
    /// Compute the `MessagePack` connection hash and EIP-712 Agent digest.
    /// # Errors
    /// Rejects vault signing (outside the supported execution scope).
    pub fn prehash(
        &self,
        nonce: u64,
        vault: Option<Address>,
        expiry: Option<DateTime<Utc>>,
        chain: Chain,
    ) -> Result<B256> {
        ensure!(vault.is_none(), "vault signing unsupported");
        let mut bytes = rmp_serde::to_vec_named(self)?;
        bytes.extend(nonce.to_be_bytes());
        bytes.push(0);
        if let Some(expiry) = expiry {
            bytes.push(0);
            bytes.extend(u64::try_from(expiry.timestamp_millis())?.to_be_bytes());
        }
        let agent = Agent {
            source: match chain {
                Chain::Mainnet => "a",
                Chain::Testnet => "b",
            }
            .into(),
            connectionId: keccak256(bytes),
        };
        Ok(agent.eip712_signing_hash(&eip712_domain! { name: "Exchange", version: "1", chain_id: 1337, verifying_contract: Address::ZERO, }))
    }
    /// Sign an exact supported action using a caller-owned signer.
    /// # Errors
    /// Propagates hashing and signer failures.
    pub fn sign_sync<S: DigestSigner>(
        &self,
        signer: &S,
        nonce: u64,
        vault: Option<Address>,
        expiry: Option<DateTime<Utc>>,
        chain: Chain,
    ) -> Result<ActionRequest> {
        let hash = self.prehash(nonce, vault, expiry, chain)?;
        let signature = signer.sign_hash_sync(&hash)?;
        ensure!(
            signature.recover_address_from_prehash(&hash)? == signer.address(),
            "signer identity mismatch"
        );
        Ok(ActionRequest {
            action: self.clone(),
            nonce,
            signature: signature.into(),
            vault_address: vault,
            expires_after: expiry
                .map(|e| u64::try_from(e.timestamp_millis()))
                .transpose()?,
        })
    }
    /// Recover the identity bound to the exact action, nonce, expiry and network.
    /// # Errors
    /// Rejects invalid scalars, high-S signatures, and non-Ethereum recovery IDs.
    pub fn recover(
        &self,
        signature: &WireSignature,
        nonce: u64,
        vault: Option<Address>,
        expiry: Option<DateTime<Utc>>,
        chain: Chain,
    ) -> Result<Address> {
        ensure!(matches!(signature.v, 27 | 28), "invalid recovery parity");
        Signature::new(signature.r, signature.s, signature.v == 28)
            .recover_address_from_prehash(&self.prehash(nonce, vault, expiry, chain)?)
    }
}
/// Recovery uses modern k256 exclusively; Alloy supplies only public wire types.
pub trait SignatureRecovery {
    /// Recover a low-S signature against an already hashed digest.
    /// # Errors
    /// Rejects invalid/noncanonical signatures or failed recovery.
    fn recover_address_from_prehash(&self, hash: &B256) -> Result<Address>;
}
impl SignatureRecovery for Signature {
    fn recover_address_from_prehash(&self, hash: &B256) -> Result<Address> {
        let sig = k256::ecdsa::Signature::from_scalars(
            self.r().to_be_bytes::<32>(),
            self.s().to_be_bytes::<32>(),
        )?;
        ensure!(sig.normalize_s() == sig, "noncanonical high-S signature");
        let id = k256::ecdsa::RecoveryId::new(self.v(), false);
        let key = k256::ecdsa::VerifyingKey::recover_from_prehash(hash.as_slice(), &sig, id)?;
        Ok(address(&key))
    }
}
fn address(key: &k256::ecdsa::VerifyingKey) -> Address {
    let point = key.to_sec1_point(false);
    Address::from_slice(&keccak256(&point.as_bytes()[1..])[12..])
}
/// Derive an Ethereum address only from a valid secp256k1 SEC1 public point.
/// # Errors
/// Rejects malformed encodings, infinity and coordinates outside the curve.
pub fn address_from_sec1(bytes: &[u8]) -> Result<Address> {
    Ok(address(&k256::ecdsa::VerifyingKey::from_sec1_bytes(bytes)?))
}
/// Host-supplied digest signer. Production custody stays outside this crate.
pub trait DigestSigner {
    fn address(&self) -> Address;
    /// # Errors
    /// Returns a secret-free error if the signer refuses the digest.
    fn sign_hash_sync(&self, hash: &B256) -> Result<Signature>;
}
impl DigestSigner for SigningKey {
    fn address(&self) -> Address {
        address(self.verifying_key())
    }
    fn sign_hash_sync(&self, hash: &B256) -> Result<Signature> {
        let (sig, id) = self.sign_prehash_recoverable(hash.as_slice());
        ensure!(!id.is_x_reduced(), "unsupported recovery ID");
        let bytes = sig.to_bytes();
        Ok(Signature::new(
            U256::from_be_slice(&bytes[..32]),
            U256::from_be_slice(&bytes[32..]),
            id.is_y_odd(),
        ))
    }
}

mod cloid {
    use alloy_primitives::B128;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(value: &B128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<B128, D::Error> {
        String::deserialize(d)?.parse().map_err(D::Error::custom)
    }
}
mod optional_cloid {
    use alloy_primitives::B128;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    #[allow(clippy::ref_option)] // Required by serde with.
    pub fn serialize<S: Serializer>(value: &Option<B128>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(v) => super::cloid::serialize(v, s),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<B128>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|s| s.parse().map_err(D::Error::custom))
            .transpose()
    }
}
